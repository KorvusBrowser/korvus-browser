//! THE COOKIE STORE: one store, Chrome 150's (`net::CookieMonster`), behind every view of it -- the `Cookie` header a
//! request carries and the `Set-Cookie` its response stores, `document.cookie`, and the `cookieStore` API.
//!
//! WHY ONE. There were three: wreq's `Jar` sent and stored the network's cookies, a read model beside it parsed every
//! response's `Set-Cookie` again for `document.cookie`, and `cookieStore` kept a third, per-realm list of its own. They
//! disagreed: the read model took every
//! response's cookies -- a `Domain` that did not domain-match the host that sent it (Chrome:
//! `EXCLUDE_DOMAIN_MISMATCH`), a `credentials: 'omit'` response's -- so `document.cookie` showed cookies the network never
//! sent; neither checked a `Domain` against the public suffix list (`domain=localhost`, `domain=appspot.com` stored);
//! and `cookieStore` saw none of the page's cookies. Measured on stock Chrome 150 (`core/korvus-runtime/tests/
//! fixtures/cookies`, three runs alike), korvus differed in 19 of its 36 rows.
//!
//! WHAT IS CHROMIUM'S HERE, each from its own source:
//!
//! ```text
//! the Set-Cookie line's grammar            net/cookies/parsed_cookie.cc          ParsedCookie
//! a cookie from a line / from cookieStore  net/cookies/canonical_cookie.cc       Create, CreateSanitizedCookie, ParseExpiration,
//!                                                                                ValidateAndAdjustExpiryDate (400 days)
//! Domain, Path, prefixes, Partitioned      net/cookies/cookie_util.cc            GetCookieDomainWithString (the registry --
//!                                                                                public suffix -- rule), CanonPathWithString,
//!                                                                                IsCookiePrefixValid, HasHiddenPrefixName,
//!                                                                                IsCookiePartitionedValid, ParseCookieExpirationTime
//! who may set and who is sent a cookie     net/cookies/cookie_base.cc            IsSetPermittedInContext, IncludeForRequestURL,
//!                                                                                GetEffectiveSameSite (Lax-allowing-unsafe, 2 min)
//! the SameSite context of an access        net/cookies/cookie_util.cc            ComputeSameSiteContextFor{Request,Response,
//!                                                                                ScriptGet,ScriptSet}
//! storing and selecting                    net/cookies/cookie_monster.cc         SetCanonicalCookie, MaybeDeleteEquivalentCookie-
//!                                                                                AndUpdateStatus (leave Secure cookies alone,
//!                                                                                HttpOnly from script, the creation date an
//!                                                                                unchanged value keeps), CookieSorter
//! the partition of a partitioned cookie    net/cookies/cookie_partition_key.cc   FromNetworkIsolationKey (top-level site, and
//!                                                                                whether the context is cross-site to it)
//! cookieStore's items and change events    blink/.../cookie_store/cookie_change_event.cc, cookie_store.cc,
//!                                          services/network/restricted_cookie_manager.cc
//! ```
//!
//! Chrome 150's defaults decide the feature switches: `kCookieParseRejectEmptyNameAmbiguous` on, scheme- and port-bound
//! cookies off (their checks only warn), `kDisallowNonAsciiCookies` and time-limited insecure cookies off. Not carried:
//! the per-domain and global garbage collection limits (180 cookies a domain, 3300 in all) and cookie priorities, which
//! only that collection reads; a response's `Clear-Site-Data`.
//!
//! THE HARNESS. An engine handed a document as a string (`Engine::load_html`) has no URL -- its cookie URL is
//! `about:blank`, which no browser navigation produces for a document that touches cookies. Its documents keep their
//! own store (the realm's [`CookieMonster`], never the process's): a host-less, path-less, secure scope where every
//! cookie it writes applies, as the read model treated it before.

use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::context_store::Synced;

// ── the clock ────────────────────────────────────────────────────────────────────────────────────────

/// Microseconds since the Unix epoch: the store's clock. (Chrome's `base::Time` counts from 1601; only differences,
/// and `cookieStore`'s millisecond `expires`, are observable.)
pub type Micros = i64;

const MICROS_PER_SECOND: i64 = 1_000_000;
/// `base::Time::Min()`: what `Max-Age` <= 0 expires a cookie at.
const TIME_MIN: Micros = i64::MIN;
/// `kLaxAllowUnsafeMaxAge`: how long a cookie without `SameSite` still goes out on a cross-site top-level POST.
const LAX_ALLOW_UNSAFE_MAX_AGE: Micros = 2 * 60 * MICROS_PER_SECOND;
/// `ValidateAndAdjustExpiryDate`'s cap: no cookie outlives 400 days from its creation.
const MAX_EXPIRY_AGE: Micros = 400 * 24 * 3600 * MICROS_PER_SECOND;

fn now() -> Micros {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_micros() as i64,
        Err(e) => -(e.duration().as_micros() as i64),
    }
}

// ── ParsedCookie (net/cookies/parsed_cookie.cc) ──────────────────────────────────────────────────────

/// `kMaxCookieNamePlusValueSize`.
const MAX_NAME_PLUS_VALUE: usize = 4096;
/// `kMaxCookieAttributeValueSize`: a longer attribute value is ignored, the cookie kept.
const MAX_ATTRIBUTE_VALUE: usize = 1024;
const WHITESPACE: &[u8] = b" \t";

/// `HttpUtil::IsControlChar`.
fn is_control(c: u8) -> bool {
    c <= 0x1f || c == 0x7f
}

/// `ParsedCookie::IsValidCookieName`: no CTL, `;` or `=`.
fn valid_cookie_name(s: &str) -> bool {
    s.bytes().all(|c| !is_control(c) && c != b';' && c != b'=')
}

/// `ParsedCookie::IsValidCookieValue` (and an attribute value's character set): no CTL or `;`.
fn valid_cookie_value(s: &str) -> bool {
    s.bytes().all(|c| !is_control(c) && c != b';')
}

/// `ParsedCookie::IsValidCookieNameValuePair`.
fn valid_name_value_pair(name: &str, value: &str) -> bool {
    !(name.is_empty() && value.is_empty())
        && name.len() + value.len() <= MAX_NAME_PLUS_VALUE
        && valid_cookie_name(name)
        && valid_cookie_value(value)
}

/// `ParsedCookie::ParseToken`: from `*it` past leading whitespace to the next `;` or `=`, the token without trailing
/// whitespace. `None` (and `*it == end`) when only whitespace was left.
fn parse_token(b: &[u8], it: &mut usize, end: usize) -> Option<(usize, usize)> {
    while *it < end && WHITESPACE.contains(&b[*it]) {
        *it += 1;
    }
    if *it == end {
        return None;
    }
    let start = *it;
    while *it < end && b[*it] != b';' && b[*it] != b'=' {
        *it += 1;
    }
    let mut token_end = *it;
    while token_end > start && WHITESPACE.contains(&b[token_end - 1]) {
        token_end -= 1;
    }
    Some((start, token_end))
}

/// `ParsedCookie::ParseValue`: from `*it` past leading whitespace to the next `;` (`=` allowed), without trailing
/// whitespace.
fn parse_value(b: &[u8], it: &mut usize, end: usize) -> (usize, usize) {
    while *it < end && WHITESPACE.contains(&b[*it]) {
        *it += 1;
    }
    let start = *it;
    while *it < end && b[*it] != b';' {
        *it += 1;
    }
    let mut value_end = *it;
    while value_end > start && WHITESPACE.contains(&b[value_end - 1]) {
        value_end -= 1;
    }
    (start, value_end)
}

/// `ParsedCookie::ParseValueString`: what a value parses to, alone.
fn parse_value_string(s: &str) -> &str {
    let b = s.as_bytes();
    let end = b.iter().position(|c| matches!(c, b'\n' | b'\r' | 0)).unwrap_or(b.len());
    let mut it = 0;
    let (start, stop) = parse_value(b, &mut it, end);
    &s[start..stop]
}

/// `ParsedCookie::ParseTokenString`: what a name parses to, alone.
fn parse_token_string(s: &str) -> &str {
    let b = s.as_bytes();
    let end = b.iter().position(|c| matches!(c, b'\n' | b'\r' | 0)).unwrap_or(b.len());
    let mut it = 0;
    match parse_token(b, &mut it, end) {
        Some((start, stop)) => &s[start..stop],
        None => "",
    }
}

/// A `Set-Cookie` line (or a `document.cookie` write) as `net::ParsedCookie` reads it: its name/value pair, then its
/// attributes, the LAST of each known one winning.
#[derive(Debug)]
struct ParsedCookie {
    pairs: Vec<(String, String)>,
    path: usize,
    domain: usize,
    expires: usize,
    max_age: usize,
    secure: usize,
    http_only: usize,
    same_site: usize,
    partitioned: usize,
}

impl ParsedCookie {
    /// `ParsedCookie::ParsedCookie` + `IsValid`: `None` for a line no cookie comes of.
    fn parse(line: &str) -> Option<ParsedCookie> {
        let b = line.as_bytes();
        // a line a terminator cut short is refused, not truncated (EXCLUDE_DISALLOWED_CHARACTER)
        if b.iter().any(|c| matches!(c, b'\n' | b'\r' | 0)) || b.is_empty() {
            return None;
        }
        let end = b.len();
        let mut pairs: Vec<(String, String)> = Vec::new();
        let mut it = 0usize;
        let mut pair_num = 0usize;
        while it != end {
            let token = parse_token(b, &mut it, end);
            let (token_start, token_end) = match token {
                Some(t) => t,
                // the first token may be empty: its value starts at the very beginning
                None if pair_num == 0 => (0, 0),
                None => break,
            };
            let name;
            if it == end || b[it] != b'=' {
                if pair_num == 0 {
                    // a lone value is a value with an empty name (Mozilla bug 169091): `AAA` and `AAA=1` are two cookies
                    name = String::new();
                    it = token_start;
                } else {
                    // `secure;` is a name with an empty value
                    name = line[token_start..token_end].to_string();
                }
            } else {
                name = line[token_start..token_end].to_string();
                it += 1;
            }
            let (value_start, value_end) = parse_value(b, &mut it, end);
            let value = line[value_start..value_end].to_string();
            if pair_num == 0 {
                if !valid_name_value_pair(&name, &value) {
                    return None;
                }
                pairs.push((name, value));
            } else {
                // attribute names are case-insensitive (RFC 2109); a bad character in one refuses the line, an
                // oversized value drops the attribute alone
                let name = name.to_ascii_lowercase();
                if !valid_cookie_name(&name) || !valid_cookie_value(&value) {
                    return None;
                }
                if value.len() <= MAX_ATTRIBUTE_VALUE {
                    pairs.push((name, value));
                }
            }
            if it != end {
                it += 1;
            }
            pair_num += 1;
        }
        if pairs.is_empty() {
            return None;
        }
        let mut pc = ParsedCookie { pairs, path: 0, domain: 0, expires: 0, max_age: 0, secure: 0, http_only: 0, same_site: 0, partitioned: 0 };
        for i in 1..pc.pairs.len() {
            match pc.pairs[i].0.as_str() {
                "path" => pc.path = i,
                "domain" => pc.domain = i,
                "expires" => pc.expires = i,
                "max-age" => pc.max_age = i,
                "secure" => pc.secure = i,
                "httponly" => pc.http_only = i,
                "samesite" => pc.same_site = i,
                "partitioned" => pc.partitioned = i,
                _ => {}
            }
        }
        Some(pc)
    }

    fn name(&self) -> &str {
        &self.pairs[0].0
    }

    fn value(&self) -> &str {
        &self.pairs[0].1
    }

    fn attr(&self, i: usize) -> Option<&str> {
        (i != 0).then(|| self.pairs[i].1.as_str())
    }

    fn secure(&self) -> bool {
        self.secure != 0
    }

    fn http_only(&self) -> bool {
        self.http_only != 0
    }

    fn partitioned(&self) -> bool {
        self.partitioned != 0
    }

    fn same_site(&self) -> SameSite {
        self.attr(self.same_site).map_or(SameSite::Unspecified, same_site_of)
    }
}

// ── the attributes ───────────────────────────────────────────────────────────────────────────────────

/// `net::CookieSameSite`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum SameSite {
    Unspecified,
    NoRestriction,
    Lax,
    Strict,
}

/// `StringToCookieSameSite`: an unrecognized value is no value.
fn same_site_of(s: &str) -> SameSite {
    if s.eq_ignore_ascii_case("none") {
        SameSite::NoRestriction
    } else if s.eq_ignore_ascii_case("lax") {
        SameSite::Lax
    } else if s.eq_ignore_ascii_case("strict") {
        SameSite::Strict
    } else {
        SameSite::Unspecified
    }
}

/// `net::CookieEffectiveSameSite`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effective {
    NoRestriction,
    Lax,
    Strict,
    LaxAllowUnsafe,
}

/// `CookieOptions::SameSiteCookieContext::ContextType`, least trusted first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Context {
    CrossSite,
    LaxMethodUnsafe,
    Lax,
    Strict,
}

/// `net::CookieAccessScheme`: whether a URL may see `Secure` cookies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    NonCryptographic,
    /// `localhost`, `*.localhost`, a loopback address over `http:` (`cookie_util::ProvisionalAccessScheme`).
    Trustworthy,
    Cryptographic,
}

/// A cookie's partition (`net::CookiePartitionKey`): the top-level site, and whether the context that set it -- the
/// frame, or the request -- is cross-site to it (the ancestor chain bit).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PartitionKey {
    pub top_site: String,
    pub cross_site: bool,
}

impl PartitionKey {
    /// The partition of a context whose top-level document is at `top_level` (`None`: unknown, so none).
    fn of(top_level: Option<&url::Url>, cross_site: bool) -> Option<PartitionKey> {
        let top_site = crate::schemeful_site(top_level?)?;
        Some(PartitionKey { top_site, cross_site })
    }
}

// ── the URL a cookie is set from or read for ─────────────────────────────────────────────────────────

/// A URL as the store reads it (GURL's `host()`, `path()` and scheme facts).
#[derive(Clone, Debug)]
struct CookieUrl {
    scheme: String,
    /// The canonical host; an IPv6 literal bracketed, as GURL's `host()` is.
    host: String,
    path: String,
    is_ip: bool,
    /// `http`, `https`, `ws`, `wss`: `CookieMonster::kDefaultCookieableSchemes`.
    cookieable: bool,
    /// GURL's `IsStandard()`.
    standard: bool,
    access: Access,
    /// The engine-from-a-string document (module docs).
    harness: bool,
    /// The URL's port, or the scheme's default; -1 for none. A cookie remembers the one it was set from (CDP's
    /// `Network.Cookie.sourcePort`).
    port: i32,
}

impl CookieUrl {
    fn from_url(u: &url::Url) -> CookieUrl {
        let scheme = u.scheme().to_ascii_lowercase();
        let host = u.host_str().unwrap_or("").to_ascii_lowercase();
        let is_ip = matches!(u.host(), Some(url::Host::Ipv4(_)) | Some(url::Host::Ipv6(_)));
        let cookieable = matches!(scheme.as_str(), "http" | "https" | "ws" | "wss");
        let standard = matches!(scheme.as_str(), "http" | "https" | "ws" | "wss" | "ftp" | "file");
        let access = if matches!(scheme.as_str(), "https" | "wss") {
            Access::Cryptographic
        } else if is_localhost(u) {
            Access::Trustworthy
        } else {
            Access::NonCryptographic
        };
        let port = u.port_or_known_default().map(i32::from).unwrap_or(-1);
        CookieUrl { scheme, host, path: u.path().to_string(), is_ip, cookieable, standard, access, harness: false, port }
    }

    fn harness() -> CookieUrl {
        CookieUrl {
            scheme: String::new(),
            host: String::new(),
            path: "/".to_string(),
            is_ip: false,
            cookieable: true,
            standard: true,
            access: Access::Cryptographic,
            harness: true,
            port: -1,
        }
    }
}

/// `net::IsLocalhost`: a loopback address, `localhost`, or a name under `.localhost` (one trailing dot ignored).
fn is_localhost(u: &url::Url) -> bool {
    match u.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(d)) => {
            let d = d.strip_suffix('.').unwrap_or(d).to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost")
        }
        None => false,
    }
}

// ── cookie_util ──────────────────────────────────────────────────────────────────────────────────────

/// `CookieDomainAsHost`.
fn domain_as_host(domain: &str) -> &str {
    domain.strip_prefix('.').unwrap_or(domain)
}

/// Whether `s` is an IP literal (`url::CanonHostInfo::IsIPAddress`).
fn is_ip_literal(s: &str) -> bool {
    matches!(url::Host::parse(s), Ok(url::Host::Ipv4(_)) | Ok(url::Host::Ipv6(_)))
}

/// `net::CanonicalizeHost` of a `Domain` attribute's value: lower-cased, IDNA to ASCII, an IP literal canonical; its
/// one leading dot kept. `None` when it does not canonicalize.
fn canonicalize_domain(s: &str) -> Option<String> {
    let (dot, rest) = match s.strip_prefix('.') {
        Some(r) => (".", r),
        None => ("", s),
    };
    if rest.is_empty() || rest.starts_with('.') {
        return None;
    }
    let host = match url::Host::parse(rest).ok()? {
        url::Host::Domain(d) => d,
        url::Host::Ipv4(a) => a.to_string(),
        url::Host::Ipv6(a) => format!("[{a}]"),
    };
    Some(format!("{dot}{host}"))
}

/// `registry_controlled_domains::GetDomainAndRegistry(host, INCLUDE_PRIVATE_REGISTRIES)`: the registrable domain, an
/// unknown top-level label counting as a registry (`a.rv3.localhost` -> `rv3.localhost`); `""` for an IP literal or a
/// host that is a registry itself (`localhost`, `co.uk`, `appspot.com`). Leading dots are skipped.
fn domain_and_registry(host: &str) -> String {
    let h = host.trim_start_matches('.');
    if h.is_empty() || is_ip_literal(h) {
        return String::new();
    }
    psl::domain_str(h).map(str::to_string).unwrap_or_default()
}

/// `cookie_util::GetEffectiveDomain`.
fn effective_domain(scheme: &str, host: &str) -> String {
    if matches!(scheme, "http" | "https" | "ws" | "wss") {
        domain_and_registry(host)
    } else {
        domain_as_host(host).to_string()
    }
}

/// `cookie_util::GetCookieDomainWithString`: the cookie's domain -- the host for a host-only cookie, `.domain` for a
/// domain cookie -- or `None` when the `Domain` attribute may not be used from `url` (another site, a public suffix, a
/// host it does not cover, an IP literal it does not name).
fn cookie_domain_with_string(url: &CookieUrl, domain_string: &str) -> Option<String> {
    let url_host = url.host.as_str();
    if url_host.ends_with("..") {
        return None;
    }
    let domain_matches_host = url_host.eq_ignore_ascii_case(domain_string)
        || domain_string.strip_prefix('.').is_some_and(|d| url_host.eq_ignore_ascii_case(d));
    // No Domain: a host cookie. An IP host may name itself (a leading dot ignored): a host cookie too.
    if domain_string.is_empty() || (url.is_ip && domain_matches_host) {
        if url.standard {
            return Some(url_host.to_string());
        }
        return (!url_host.starts_with('.')).then(|| url_host.to_string());
    } else if url.is_ip {
        return None;
    }
    // no %-escaped domain
    if domain_string.contains('%') {
        return None;
    }
    let canon = canonicalize_domain(domain_string)?;
    let cookie_domain = if canon.starts_with('.') { canon } else { format!(".{canon}") };
    let url_domain_and_registry = effective_domain(&url.scheme, url_host);
    if url_domain_and_registry.is_empty() {
        // an IP address, an intranet host or a public suffix can set no domain cookie; naming itself, a host cookie
        let normalized = domain_string.strip_prefix('.').unwrap_or(domain_string).to_ascii_lowercase();
        return (url_host == normalized).then_some(normalized);
    }
    // the same registrable domain (EXCLUDE_DOMAIN_MISMATCH)...
    if url_domain_and_registry != effective_domain(&url.scheme, &cookie_domain) {
        return None;
    }
    // ... and the host is the domain or under it
    let not_suffix = if url_host.len() < cookie_domain.len() {
        cookie_domain[1..] != *url_host
    } else {
        !url_host.ends_with(cookie_domain.as_str())
    };
    if not_suffix {
        return None;
    }
    Some(cookie_domain)
}

/// `cookie_util::CanonPathWithString`: a `Path` that starts with `/` as given; else the URL's directory.
fn canon_path(url: &CookieUrl, path_string: &str) -> String {
    if path_string.starts_with('/') {
        return path_string.to_string();
    }
    match url.path.rfind('/') {
        None | Some(0) => "/".to_string(),
        Some(i) => url.path[..i].to_string(),
    }
}

/// `cookie_util::IsDomainMatch`.
fn domain_match(domain: &str, host: &str) -> bool {
    if host == domain {
        return true;
    }
    if !domain.starts_with('.') {
        return false;
    }
    if domain[1..] == *host {
        return true;
    }
    host.len() > domain.len() && host.ends_with(domain)
}

/// `cookie_util::IsOnPath`.
fn is_on_path(cookie_path: &str, url_path: &str) -> bool {
    if cookie_path.is_empty() || !url_path.starts_with(cookie_path) {
        return false;
    }
    cookie_path.len() == url_path.len() || cookie_path.ends_with('/') || url_path.as_bytes()[cookie_path.len()] == b'/'
}

/// `net::IsSubdomainOf`.
fn is_subdomain_of(sub: &str, sup: &str) -> bool {
    if sub.len() <= sup.len() {
        return sub == sup;
    }
    sub.ends_with(sup) && sub.as_bytes()[sub.len() - sup.len() - 1] == b'.'
}

/// `net::CookiePrefix`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Prefix {
    None,
    Secure,
    Host,
    Http,
    HostHttp,
}

/// `kPrefixes`, in the order `GetCookiePrefix` tries them (case-insensitive).
const PREFIXES: [(&str, Prefix); 4] = [("__Secure-", Prefix::Secure), ("__Host-Http-", Prefix::HostHttp), ("__Http-", Prefix::Http), ("__Host-", Prefix::Host)];

fn starts_with_ignore_ascii_case(s: &str, p: &str) -> bool {
    s.len() >= p.len() && s.as_bytes()[..p.len()].eq_ignore_ascii_case(p.as_bytes())
}

/// `cookie_util::GetCookiePrefix`.
fn prefix_of(name: &str) -> Prefix {
    PREFIXES.iter().find(|(p, _)| starts_with_ignore_ascii_case(name, p)).map_or(Prefix::None, |(_, t)| *t)
}

/// `cookie_util::HasHiddenPrefixName`: a nameless cookie whose value reads as a prefixed name.
fn has_hidden_prefix_name(value: &str) -> bool {
    let v = value.trim_start_matches([' ', '\t']);
    PREFIXES.iter().any(|(p, _)| starts_with_ignore_ascii_case(v, p))
}

/// `cookie_util::IsCookiePrefixValid` with the URL: `domain` and `path` are the raw attribute values.
fn prefix_valid(prefix: Prefix, url: &CookieUrl, secure: bool, http_only: bool, domain: &str, path: &str) -> bool {
    let secure_ok = secure && url.access != Access::NonCryptographic;
    let host_ok = secure_ok && path == "/" && (domain.is_empty() || (url.is_ip && url.host == domain));
    match prefix {
        Prefix::None => true,
        Prefix::Secure => secure_ok,
        Prefix::Host => host_ok,
        Prefix::Http => secure_ok && http_only,
        Prefix::HostHttp => secure_ok && http_only && host_ok,
    }
}

/// `base::StringToInt64`: the value and whether the conversion was perfect (an overflow saturates, unconverted).
fn string_to_i64(s: &str) -> (i64, bool) {
    let b = s.as_bytes();
    let mut i = 0;
    let mut leading_ws = false;
    while i < b.len() && b[i].is_ascii_whitespace() {
        leading_ws = true;
        i += 1;
    }
    let negative = i < b.len() && b[i] == b'-';
    if negative || (i < b.len() && b[i] == b'+') {
        i += 1;
    }
    if i == b.len() {
        return (0, false);
    }
    let mut v: i64 = 0;
    for (n, &c) in b[i..].iter().enumerate() {
        if !c.is_ascii_digit() {
            return (v, false);
        }
        let d = (c - b'0') as i64;
        if n > 0 {
            v = match v.checked_mul(10) {
                Some(x) => x,
                None => return (if negative { i64::MIN } else { i64::MAX }, false),
            };
        }
        v = match if negative { v.checked_sub(d) } else { v.checked_add(d) } {
            Some(x) => x,
            None => return (if negative { i64::MIN } else { i64::MAX }, false),
        };
    }
    (v, !leading_ws)
}

/// Days from 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

/// `cookie_util::ParseCookieExpirationTime`: Chrome's lenient `Expires` reader -- a month name (any word starting with
/// its first three letters), a day of at most two digits, a year of at most five (two-digit years 70-99 -> 19xx, 0-69 ->
/// 20xx), `hh:mm:ss`, in any order, between any of its delimiters. `None` when a part is missing or the date does not
/// exist (`30 Feb`, second 60: `base::Time::FromUTCExploded` refuses what does not round-trip).
fn parse_cookie_expiration_time(s: &str) -> Option<Micros> {
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    const DELIMITERS: &[u8] = b"\t !\"#$%&'()*+,-./;<=>?@[\\]^_`{|}~";
    let (mut day, mut month, mut year, mut time): (Option<i64>, Option<i64>, Option<i64>, Option<(i64, i64, i64)>) = (None, None, None, None);
    for token in s.as_bytes().split(|c| DELIMITERS.contains(c)).filter(|t| !t.is_empty()) {
        if !token[0].is_ascii_digit() {
            if month.is_none() {
                month = MONTHS.iter().position(|m| token.len() >= 3 && token[..3].eq_ignore_ascii_case(m.as_bytes())).map(|i| i as i64 + 1);
            }
        } else if token.contains(&b':') {
            if time.is_none() {
                // exactly three unsigned parts, empty parts counted (`StringTokenizer::RETURN_EMPTY_TOKENS`)
                let parts: Vec<&[u8]> = token.split(|c| *c == b':').collect();
                let part = |p: &[u8]| -> Option<i64> {
                    if p.is_empty() || !p.iter().all(u8::is_ascii_digit) {
                        return None;
                    }
                    std::str::from_utf8(p).ok()?.parse::<u32>().ok().map(i64::from)
                };
                if parts.len() == 3 {
                    if let (Some(h), Some(m), Some(sec)) = (part(parts[0]), part(parts[1]), part(parts[2])) {
                        time = Some((h, m, sec));
                    }
                }
            }
        } else if day.is_none() && token.len() <= 2 {
            if let (v, true) = string_to_i64(std::str::from_utf8(token).unwrap_or("")) {
                day = Some(v);
            }
        } else if year.is_none() && token.len() <= 5 {
            if let (v, true) = string_to_i64(std::str::from_utf8(token).unwrap_or("")) {
                year = Some(v);
            }
        }
    }
    let (Some(day), Some(month), Some((h, mi, sec)), Some(mut year)) = (day, month, time, year) else { return None };
    if (70..=99).contains(&year) {
        year += 1900;
    }
    if (0..=69).contains(&year) {
        year += 2000;
    }
    if !(1..=31).contains(&day) || h > 23 || mi > 59 || sec > 59 || day > days_in_month(year, month) {
        return None;
    }
    let secs = days_from_civil(year, month, day) * 86_400 + h * 3600 + mi * 60 + sec;
    Some(secs.saturating_mul(MICROS_PER_SECOND))
}

/// `CanonicalCookie::ParseExpiration`: `Max-Age` first (a perfect integer; <= 0 is the earliest time; one that
/// overflowed saturates), then `Expires` moved by the server's clock skew; `None` for a session cookie.
fn parse_expiration(pc: &ParsedCookie, current: Micros, server_time: Micros) -> Option<Micros> {
    if let Some(ma) = pc.attr(pc.max_age) {
        match string_to_i64(ma) {
            (v, true) => return Some(if v <= 0 { TIME_MIN } else { current.saturating_add(v.saturating_mul(MICROS_PER_SECOND)) }),
            (i64::MIN, false) => return Some(TIME_MIN),
            (i64::MAX, false) => return Some(i64::MAX),
            _ => {}
        }
    }
    let expires = pc.attr(pc.expires).filter(|e| !e.is_empty())?;
    let parsed = parse_cookie_expiration_time(expires)?;
    Some(parsed.saturating_add(current.saturating_sub(server_time)))
}

/// `CanonicalCookie::ValidateAndAdjustExpiryDate`: no later than 400 days after creation.
fn validate_expiry(expiry: Option<Micros>, creation: Micros) -> Option<Micros> {
    expiry.map(|e| e.min(creation.saturating_add(MAX_EXPIRY_AGE)))
}

// ── CanonicalCookie ──────────────────────────────────────────────────────────────────────────────────

/// A stored cookie (`net::CanonicalCookie`). `domain` is the host for a host-only cookie, `.domain` for a domain one.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CanonicalCookie {
    name: String,
    value: String,
    domain: String,
    path: String,
    creation: Micros,
    /// `None`: a session cookie.
    expiry: Option<Micros>,
    secure: bool,
    http_only: bool,
    same_site: SameSite,
    partition: Option<PartitionKey>,
    /// Where it was set from, as DevTools reports it: the port, and whether the source scheme counts as secure
    /// -- a cryptographic URL, or a trustworthy one (`localhost`) setting a `Secure` cookie (Chrome 150, recorded:
    /// `chips_c; Secure; Partitioned` from `http://localhost` reads `"Secure"`, the plain cookies beside it `"NonSecure"`).
    source_port: i32,
    source_secure: bool,
}

impl CanonicalCookie {
    /// `CanonicalCookie::Create`: the cookie a `Set-Cookie` line (or a `document.cookie` write) from `url` makes, or
    /// `None` when Chrome makes none of it.
    fn create(url: &CookieUrl, line: &str, creation: Micros, server_time: Option<Micros>, partition: Option<&PartitionKey>) -> Option<CanonicalCookie> {
        let pc = ParsedCookie::parse(line)?;
        // `=a=b`: a nameless cookie whose value reads as a pair (kCookieParseRejectEmptyNameAmbiguous)
        if pc.name().is_empty() && pc.value().contains('=') {
            return None;
        }
        let domain = cookie_domain_with_string(url, pc.attr(pc.domain).unwrap_or(""))?;
        let path = canon_path(url, pc.attr(pc.path).unwrap_or(""));
        let prefix_ok = if pc.name().is_empty() {
            !has_hidden_prefix_name(pc.value())
        } else {
            prefix_valid(prefix_of(pc.name()), url, pc.secure(), pc.http_only(), pc.attr(pc.domain).unwrap_or(""), pc.attr(pc.path).unwrap_or(""))
        };
        if !prefix_ok {
            return None;
        }
        // `Partitioned` is valid on a Secure cookie from a URL that may set one, in a context that has a partition
        let partition = match (pc.partitioned(), partition) {
            (true, Some(k)) => {
                if url.access == Access::NonCryptographic || !pc.secure() {
                    return None;
                }
                Some(k.clone())
            }
            _ => None,
        };
        let expiry = validate_expiry(parse_expiration(&pc, creation, server_time.unwrap_or(creation)), creation);
        Some(CanonicalCookie {
            name: pc.name().to_string(),
            value: pc.value().to_string(),
            domain,
            path,
            creation,
            expiry,
            secure: pc.secure(),
            http_only: pc.http_only(),
            same_site: pc.same_site(),
            partition,
            source_port: url.port,
            source_secure: url.access == Access::Cryptographic || (url.access == Access::Trustworthy && pc.secure()),
        })
    }

    fn is_expired(&self, current: Micros) -> bool {
        self.expiry.is_some_and(|e| current >= e)
    }

    fn is_domain_cookie(&self) -> bool {
        self.domain.starts_with('.')
    }

    /// `GetEffectiveSameSite` (non-legacy): no `SameSite` is Lax -- Lax that still goes out on an unsafe top-level
    /// request for its first two minutes.
    fn effective_same_site(&self, current: Micros) -> Effective {
        match self.same_site {
            SameSite::Unspecified if current.saturating_sub(self.creation) <= LAX_ALLOW_UNSAFE_MAX_AGE => Effective::LaxAllowUnsafe,
            SameSite::Unspecified | SameSite::Lax => Effective::Lax,
            SameSite::NoRestriction => Effective::NoRestriction,
            SameSite::Strict => Effective::Strict,
        }
    }

    /// `IsEquivalent` (`RefUniqueKey`, scheme- and port-binding off): the same partition, name, domain and path.
    fn same_key(&self, other: &CanonicalCookie) -> bool {
        self.partition == other.partition && self.name == other.name && self.domain == other.domain && self.path == other.path
    }

    /// `IsEquivalentForSecureCookieMatching(secure)`: the name, a domain match either way, and `secure`'s path on ours.
    fn equivalent_for_secure_matching(&self, secure: &CanonicalCookie) -> bool {
        let (mine, theirs) = (domain_as_host(&self.domain), domain_as_host(&secure.domain));
        self.partition == secure.partition
            && self.name == secure.name
            && (is_subdomain_of(mine, theirs) || is_subdomain_of(theirs, mine))
            && is_on_path(&secure.path, &self.path)
    }

    /// `IsWebEquivalentTo`: nothing a page could see differs.
    fn web_equivalent(&self, other: &CanonicalCookie) -> bool {
        self.same_key(other)
            && self.value == other.value
            && self.secure == other.secure
            && self.same_site == other.same_site
            && self.http_only == other.http_only
            && self.expiry == other.expiry
    }

    /// One entry of a cookie line (`AppendCookieLineEntry`): a nameless cookie is its value alone.
    fn line_entry(&self) -> String {
        if self.name.is_empty() {
            self.value.clone()
        } else {
            format!("{}={}", self.name, self.value)
        }
    }
}

/// `CanonicalCookie::BuildCookieLine`.
fn cookie_line(cookies: &[(CanonicalCookie, Effective)]) -> String {
    cookies.iter().map(|(c, _)| c.line_entry()).collect::<Vec<_>>().join("; ")
}

// ── the access ───────────────────────────────────────────────────────────────────────────────────────

/// What an access may do (`net::CookieOptions`): its SameSite context, and whether it is a script's (no HttpOnly).
#[derive(Clone, Copy, Debug)]
struct Options {
    context: Context,
    exclude_http_only: bool,
}

/// `CookieBase::IsSetPermittedInContext` (non-legacy).
fn set_permitted(cc: &CanonicalCookie, url: &CookieUrl, opts: &Options, current: Micros) -> bool {
    if !url.cookieable || !domain_match(&cc.domain, &url.host) {
        return false;
    }
    if url.access == Access::NonCryptographic && cc.secure {
        return false;
    }
    if opts.exclude_http_only && cc.http_only {
        return false;
    }
    if cc.same_site == SameSite::NoRestriction && !cc.secure {
        return false;
    }
    // a Strict cookie needs only a Lax context to be SET (a safe top-level cross-site navigation sets one)
    match cc.effective_same_site(current) {
        Effective::NoRestriction => true,
        _ => opts.context >= Context::Lax,
    }
}

/// `CookieBase::IncludeForRequestURL` (non-legacy): `Some` with the cookie's effective SameSite when it is sent.
fn include_for(cc: &CanonicalCookie, url: &CookieUrl, opts: &Options, current: Micros) -> Option<Effective> {
    if opts.exclude_http_only && cc.http_only {
        return None;
    }
    if url.access == Access::NonCryptographic && cc.secure {
        return None;
    }
    if !domain_match(&cc.domain, &url.host) || !is_on_path(&cc.path, &url.path) {
        return None;
    }
    let effective = cc.effective_same_site(current);
    let needed = match effective {
        Effective::Strict => Context::Strict,
        Effective::Lax => Context::Lax,
        Effective::LaxAllowUnsafe => Context::LaxMethodUnsafe,
        Effective::NoRestriction => Context::CrossSite,
    };
    if opts.context < needed {
        return None;
    }
    if cc.same_site == SameSite::NoRestriction && !cc.secure {
        return None;
    }
    Some(effective)
}

/// Why a change happened (`net::CookieChangeCause`), as far as a page can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cause {
    Inserted,
    /// An overwrite with nothing a page could see changed: no event.
    InsertedNoChangeOverwrite,
    InsertedNoValueChangeOverwrite,
    /// The cookie an insertion replaced: no event (the insertion is the event).
    Overwrite,
    /// The cookie an already-expired write deleted.
    ExpiredOverwrite,
}

/// One change a write made to the store, for `cookieStore`'s change event ([`change_items`]).
#[derive(Clone, Debug)]
pub struct Change {
    cookie: CanonicalCookie,
    cause: Cause,
}

/// The store: every cookie in insertion order (the order `CookieMonster`'s multimap keeps a host's cookies in, which
/// decides between cookies of one path length created in the same microsecond).
#[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
pub struct CookieMonster {
    cookies: Vec<CanonicalCookie>,
}

impl CookieMonster {
    /// `CookieMonster::SetCanonicalCookie`: store `cc` set from `url`, or refuse it (`None`). The changes are what a
    /// change listener would be told.
    fn set_canonical(&mut self, mut cc: CanonicalCookie, url: &CookieUrl, opts: &Options) -> Option<Vec<Change>> {
        let current = now();
        let mut include = set_permitted(&cc, url, opts, current);
        let allowed_secure = url.access != Access::NonCryptographic;
        let already_expired = cc.is_expired(cc.creation);
        // MaybeDeleteEquivalentCookieAndUpdateStatus, over the cookies of `cc`'s own partition
        let mut candidate: Option<usize> = None;
        for (i, cur) in self.cookies.iter().enumerate() {
            if cur.partition != cc.partition {
                continue;
            }
            // leave Secure cookies alone: an insecure source neither overwrites nor shadows one
            if cur.secure && !allowed_secure && cc.equivalent_for_secure_matching(cur) {
                include = false;
            }
            if cc.same_key(cur) {
                // a script cannot overwrite an HttpOnly cookie
                if opts.exclude_http_only && cur.http_only {
                    include = false;
                } else {
                    candidate = Some(i);
                }
            }
        }
        let mut cause = Cause::Inserted;
        let mut inherit: Option<Micros> = None;
        let mut changes = Vec::new();
        if let Some(i) = candidate {
            let old = &self.cookies[i];
            // an unchanged value keeps the old creation date, and so its place in the cookie line
            if old.value == cc.value {
                inherit = Some(old.creation);
                cause = Cause::InsertedNoValueChangeOverwrite;
            }
            if old.web_equivalent(&cc) {
                cause = Cause::InsertedNoChangeOverwrite;
            }
            if include {
                let old = self.cookies.remove(i);
                changes.push(Change { cookie: old, cause: if already_expired { Cause::ExpiredOverwrite } else { Cause::Overwrite } });
            }
        }
        if !include {
            return None;
        }
        if !already_expired {
            if let Some(t) = inherit {
                cc.creation = t;
            }
            changes.push(Change { cookie: cc.clone(), cause });
            self.cookies.push(cc);
        }
        Some(changes)
    }

    /// `CookieMonster::GetCookieListWithOptions`: the cookies an access to `url` gets, in `CookieSorter`'s order --
    /// longest path first, then oldest -- with each one's effective SameSite. The unpartitioned cookies and those of
    /// `partition`.
    fn get_for_url(&mut self, url: &CookieUrl, opts: &Options, partition: Option<&PartitionKey>) -> Vec<(CanonicalCookie, Effective)> {
        if !url.cookieable {
            return Vec::new();
        }
        let current = now();
        // an expired cookie is garbage the first time it is looked at
        self.cookies.retain(|c| !c.is_expired(current));
        let mut hits: Vec<&CanonicalCookie> = self.cookies.iter().filter(|c| c.partition.is_none() || c.partition.as_ref() == partition).collect();
        hits.sort_by(|a, b| b.path.len().cmp(&a.path.len()).then(a.creation.cmp(&b.creation)));
        hits.into_iter().filter_map(|c| include_for(c, url, opts, current).map(|e| (c.clone(), e))).collect()
    }
}

/// A cookie as CDP's `Network.Cookie` reports it. DevTools sees every cookie, HttpOnly ones included -- what no
/// document can. Keys and their presence follow stock Chrome 150, recorded (`~/bench-lean/cdp/probe/chrome150-a1s3.json`):
/// `sameSite` absent when unspecified, `partitionKey` only on a partitioned cookie and an object, no `sameParty`.
#[derive(Debug, Clone, PartialEq)]
pub struct DevtoolsCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    /// Seconds since the epoch; -1 for a session cookie.
    pub expires: f64,
    /// `name.len() + value.len()`, as Chrome counts it.
    pub size: usize,
    pub http_only: bool,
    pub secure: bool,
    pub session: bool,
    /// `"Strict"`, `"Lax"`, `"None"`; `None` when the cookie did not say.
    pub same_site: Option<&'static str>,
    /// `"Secure"` or `"NonSecure"`.
    pub source_scheme: &'static str,
    pub source_port: i32,
    /// `(topLevelSite, hasCrossSiteAncestor)`.
    pub partition_key: Option<(String, bool)>,
}

impl DevtoolsCookie {
    fn of(c: &CanonicalCookie) -> DevtoolsCookie {
        DevtoolsCookie {
            name: c.name.clone(),
            value: c.value.clone(),
            domain: c.domain.clone(),
            path: c.path.clone(),
            expires: c.expiry.map(|e| e as f64 / 1_000_000.0).unwrap_or(-1.0),
            size: c.name.len() + c.value.len(),
            http_only: c.http_only,
            secure: c.secure,
            session: c.expiry.is_none(),
            same_site: match c.same_site {
                SameSite::Unspecified => None,
                SameSite::NoRestriction => Some("None"),
                SameSite::Lax => Some("Lax"),
                SameSite::Strict => Some("Strict"),
            },
            source_scheme: if c.source_secure { "Secure" } else { "NonSecure" },
            source_port: c.source_port,
            partition_key: c.partition.as_ref().map(|k| (k.top_site.clone(), k.cross_site)),
        }
    }
}

/// `Network.getCookies {urls}` -- the cookies a top-level request to any of `urls` would carry, HttpOnly ones
/// included, each URL's own partition with them; `None` (`Storage.getCookies`, `Network.getAllCookies`): every
/// unexpired cookie in the store. One entry per (name, domain, path, partition), in `CookieSorter`'s order -- longest
/// path, then oldest -- which is the order of Chrome 150's `Storage.getCookies` (`CookieMonster::GetAllCookies`;
/// recorded: `d2` on `/sub` ahead of the older cookies on `/`, a partitioned cookie in its creation place). Chrome's
/// `Network.getCookies` lists them in a hash map's order (`CookieRetrieverNetworkService::all_cookies_`, an
/// `absl::flat_hash_map`), which no client can rely on; this gives that one the same order.
pub fn devtools_cookies(urls: Option<&[String]>) -> Vec<DevtoolsCookie> {
    let mut store = shared();
    devtools_list(&mut store, urls)
}

/// [`devtools_cookies`] over a context's jar as the router's store holds it (its `cookies` slot's bytes): the
/// browser-level `Storage.getCookies {browserContextId}`, answered without a page.
pub fn devtools_cookies_in(jar: &[u8]) -> Vec<DevtoolsCookie> {
    let mut store: CookieMonster = bincode::deserialize(jar).unwrap_or_default();
    devtools_list(&mut store, None)
}

/// [`devtools_set_all`] into a context's jar as the router's store holds it; `false` refuses the whole list, as Chrome
/// does. The jar's bytes are rewritten.
pub fn devtools_set_all_in(jar: &mut Vec<u8>, cs: &[DevtoolsCookieInit]) -> bool {
    let made: Option<Vec<(CanonicalCookie, CookieUrl)>> = cs.iter().map(|c| devtools_make(c).ok()).collect();
    let Some(made) = made else { return false };
    let mut store: CookieMonster = bincode::deserialize(jar).unwrap_or_default();
    for (cc, source) in made {
        store.set_canonical(cc, &source, &DEVTOOLS_WRITE);
    }
    *jar = bincode::serialize(&store).unwrap_or_default();
    true
}

/// An empty jar, as the router's store holds one (`Storage.clearCookies {browserContextId}`).
pub fn empty_jar() -> Vec<u8> {
    bincode::serialize(&CookieMonster::default()).unwrap_or_default()
}

fn devtools_list(store: &mut CookieMonster, urls: Option<&[String]>) -> Vec<DevtoolsCookie> {
    let current = now();
    store.cookies.retain(|c| !c.is_expired(current));
    let mut picked: Vec<CanonicalCookie> = match urls {
        None => store.cookies.iter().cloned().collect(),
        Some(urls) => {
            let opts = Options { context: Context::Strict, exclude_http_only: false };
            let mut out: Vec<CanonicalCookie> = Vec::new();
            for u in urls.iter().filter_map(|u| url::Url::parse(u).ok()) {
                let cu = CookieUrl::from_url(&u);
                let partition = PartitionKey::of(Some(&u), false);
                for (c, _) in store.get_for_url(&cu, &opts, partition.as_ref()) {
                    if !out.iter().any(|o| o.name == c.name && o.domain == c.domain && o.path == c.path && o.partition == c.partition) {
                        out.push(c);
                    }
                }
            }
            out
        }
    };
    picked.sort_by(|a, b| b.path.len().cmp(&a.path.len()).then(a.creation.cmp(&b.creation)));
    picked.iter().map(DevtoolsCookie::of).collect()
}

/// A cookie as `Network.setCookie(s)` / `Storage.setCookies` give it (`Network.CookieParam`).
#[derive(Debug, Clone, Default)]
pub struct DevtoolsCookieInit {
    pub name: String,
    pub value: String,
    pub url: Option<String>,
    pub domain: Option<String>,
    pub path: Option<String>,
    pub secure: bool,
    pub http_only: bool,
    /// `"Strict"`, `"Lax"`, `"None"`; anything else is unspecified.
    pub same_site: Option<String>,
    /// Seconds since the epoch; negative for a session cookie, as the protocol's default `-1`.
    pub expires: f64,
    /// `(topLevelSite, hasCrossSiteAncestor)`.
    pub partition_key: Option<(String, bool)>,
}

/// `Network.setCookie`, as Chrome 150 makes and stores one (`MakeCookieFromProtocolValues`, then
/// `SetCanonicalCookie` with every SameSite context and HttpOnly allowed). The URL is the DOMAIN's, never the given
/// URL's path: a domain with a leading dot makes a domain cookie, one without (or the URL's host) a host-only cookie on
/// it, and `secure` (or an `https:` URL) picks the scheme. `Err` carries Chrome's message for parameters that make no
/// cookie; `Ok(false)` is a cookie the store refused.
pub fn devtools_set(c: &DevtoolsCookieInit) -> Result<bool, &'static str> {
    let (cc, source) = devtools_make(c)?;
    Ok(shared().set_canonical(cc, &source, &DEVTOOLS_WRITE).is_some())
}

/// `Network.setCookies` / `Storage.setCookies`: every cookie is made first, and none is stored unless all
/// could be (Chrome 150: a list with one cookie lacking both url and domain answered `Invalid cookie fields` and stored
/// none of the list). `false` is that refusal.
pub fn devtools_set_all(cs: &[DevtoolsCookieInit]) -> bool {
    let made: Option<Vec<(CanonicalCookie, CookieUrl)>> = cs.iter().map(|c| devtools_make(c).ok()).collect();
    let Some(made) = made else { return false };
    let mut store = shared();
    for (cc, source) in made {
        store.set_canonical(cc, &source, &DEVTOOLS_WRITE);
    }
    true
}

/// What a DevTools write may do: every SameSite context, HttpOnly included (`CookieOptions` with
/// `SameSiteCookieContext::MakeInclusive()` and `set_include_httponly()`).
const DEVTOOLS_WRITE: Options = Options { context: Context::Strict, exclude_http_only: false };

/// `MakeCookieFromProtocolValues`: the cookie, and the source the store checks its write against.
fn devtools_make(c: &DevtoolsCookieInit) -> Result<(CanonicalCookie, CookieUrl), &'static str> {
    let url_spec = c.url.as_deref().unwrap_or("");
    let mut domain = c.domain.clone().unwrap_or_default();
    if url_spec.is_empty() && domain.is_empty() {
        return Err("At least one of the url or domain needs to be specified");
    }
    let mut secure = c.secure;
    if !url_spec.is_empty() {
        let u = url::Url::parse(url_spec).ok().filter(|u| matches!(u.scheme(), "http" | "https")).ok_or("URL must have scheme http or https")?;
        secure = secure || u.scheme() == "https";
        if domain.is_empty() {
            domain = u.host_str().unwrap_or("").to_string();
        }
    }
    let (url_host, cookie_domain) = match domain.strip_prefix('.') {
        Some(h) => (h.to_string(), Some(domain.clone())),
        None => (domain.clone(), None),
    };
    const SANITIZE: &str = "Sanitizing cookie failed";
    if !valid_cookie_name(&c.name) || !valid_cookie_value(&c.value) {
        return Err(SANITIZE);
    }
    let u = url::Url::parse(&format!("{}://{url_host}", if secure { "https" } else { "http" })).map_err(|_| SANITIZE)?;
    let mut line = format!("{}={}", c.name, c.value);
    if let Some(d) = &cookie_domain {
        line.push_str("; Domain=");
        line.push_str(d);
    }
    if let Some(p) = c.path.as_deref().filter(|p| !p.is_empty()) {
        if !p.starts_with('/') || p.contains(';') {
            return Err(SANITIZE);
        }
        line.push_str("; Path=");
        line.push_str(p);
    }
    if secure {
        line.push_str("; Secure");
    }
    if c.http_only {
        line.push_str("; HttpOnly");
    }
    match c.same_site.as_deref() {
        Some("Strict") => line.push_str("; SameSite=Strict"),
        Some("Lax") => line.push_str("; SameSite=Lax"),
        Some("None") => line.push_str("; SameSite=None"),
        _ => {}
    }
    let partition = c.partition_key.as_ref().map(|(site, cross)| PartitionKey { top_site: site.clone(), cross_site: *cross });
    if partition.is_some() {
        line.push_str("; Partitioned");
    }
    let creation = now();
    let mut cc = CanonicalCookie::create(&CookieUrl::from_url(&u), &line, creation, None, partition.as_ref()).ok_or(SANITIZE)?;
    // `expires` 0 is the epoch -- already expired, which deletes the cookie it matches
    if c.expires >= 0.0 {
        cc.expiry = validate_expiry(Some((c.expires * 1_000_000.0) as Micros), creation);
    }
    // the write is checked against the cookie's simulated source, `https://<domain><path>` (`SimulatedCookieSource`)
    let source = url::Url::parse(&format!("https://{url_host}{}", cc.path)).map_err(|_| SANITIZE)?;
    Ok((cc, CookieUrl::from_url(&source)))
}

/// `Network.deleteCookies` (Chrome 150 `DeleteFilteredCookies`): every cookie named `name` whose domain is
/// exactly `domain` -- or the URL's host when no domain is given -- on `path` when one is given, and in `partition`
/// (none: only unpartitioned cookies).
pub fn devtools_delete(name: &str, url: Option<&str>, domain: Option<&str>, path: Option<&str>, partition: Option<(String, bool)>) -> Result<(), &'static str> {
    if url.is_none() && domain.is_none() {
        return Err("At least one of the url and domain needs to be specified");
    }
    let mut d = domain.unwrap_or("").to_string();
    if d.is_empty() {
        let u = url::Url::parse(url.unwrap_or("")).ok().filter(|u| matches!(u.scheme(), "http" | "https")).ok_or("An http or https url URL must be specified")?;
        d = u.host_str().unwrap_or("").to_string();
    }
    let key = partition.map(|(top_site, cross_site)| PartitionKey { top_site, cross_site });
    shared().cookies.retain(|c| {
        let hit = c.name == name && c.domain == d && path.is_none_or(|p| p.is_empty() || c.path == p) && c.partition == key;
        !hit
    });
    Ok(())
}

/// The context's store: what the network sends and stores, and what every document with a URL reads and writes.
/// It is this process's own unless the process is a page of a browser endpoint's context, when it is the context's,
/// shared with the context's other pages through the router ([`crate::context_store`]): every use takes the jar's
/// lock in the router and brings the cookies current, and a use that changed them writes them back as it ends.
fn shared() -> SharedJar {
    static STORE: OnceLock<Mutex<Synced<CookieMonster>>> = OnceLock::new();
    let mut guard = STORE.get_or_init(|| Mutex::new(Synced::new("cookies"))).lock().unwrap_or_else(|e| e.into_inner());
    let locked = guard.begin();
    SharedJar { guard, locked }
}

/// [`shared`]'s guard: the jar, written back to the context and unlocked when it drops.
struct SharedJar {
    guard: MutexGuard<'static, Synced<CookieMonster>>,
    locked: bool,
}

impl std::ops::Deref for SharedJar {
    type Target = CookieMonster;
    fn deref(&self) -> &CookieMonster {
        self.guard.value()
    }
}

impl std::ops::DerefMut for SharedJar {
    fn deref_mut(&mut self) -> &mut CookieMonster {
        self.guard.value_mut()
    }
}

impl Drop for SharedJar {
    fn drop(&mut self) {
        let locked = self.locked;
        self.guard.end(locked);
    }
}

/// Drop every cookie (test isolation).
pub fn clear() {
    shared().cookies.clear();
}

// ── a request's hop ──────────────────────────────────────────────────────────────────────────────────

/// How one hop of a request meets the store: the SameSite context its cookies are sent in, the one its response's
/// are stored in, and its partition. `ComputeSameSiteContextForRequest` / `ForResponse` and
/// `CookiePartitionKey::FromNetworkIsolationKey`, from what the request knows of the document that asked.
#[derive(Clone, Debug)]
pub struct HopContext {
    get: Context,
    set: Context,
    partition: Option<PartitionKey>,
}

impl HopContext {
    /// The context of `req`'s hop to `target`, `redirected` when a redirect led there.
    ///
    /// A TOP-LEVEL navigation is its own site for cookies: strictly same-site when its initiator is same-site with the
    /// target (or there is none: the user), else Lax -- Lax with an unsafe method for a cross-site POST -- and it may
    /// set every cookie; its partition is the target's site. ANY OTHER request goes out under the requesting
    /// document's site for cookies: [`FetchRequest::cookie_cross_site`] says whether the target is cross-site to it,
    /// and an initiator cross-site to the target makes the context cross-site too (as it does at a hop a redirect
    /// took there, where the initiator stands for the site for cookies). Its partition: the top-level site, and
    /// whether the target is cross-site to the site for cookies.
    pub(crate) fn for_hop(req: &crate::FetchRequest, initiator: Option<&url::Url>, top_level: Option<&url::Url>, target: &url::Url, method: &str, redirected: bool) -> HopContext {
        let web = |u: &&url::Url| matches!(u.scheme(), "http" | "https" | "ws" | "wss");
        let initiator_cross = initiator.filter(web).is_some_and(|i| !crate::same_site(i, target));
        if req.mode == crate::Mode::Navigate && req.destination.token() == "document" {
            let safe = matches!(method, "GET" | "HEAD" | "OPTIONS" | "TRACE");
            let get = match (initiator_cross, safe) {
                (false, _) => Context::Strict,
                (true, true) => Context::Lax,
                (true, false) => Context::LaxMethodUnsafe,
            };
            return HopContext { get, set: Context::Lax, partition: PartitionKey::of(Some(target), false) };
        }
        let sfc_cross = req.cookie_cross_site || (redirected && initiator_cross);
        let cross = sfc_cross || initiator_cross;
        let top = top_level.or(initiator).unwrap_or(target);
        HopContext {
            get: if cross { Context::CrossSite } else { Context::Strict },
            set: if cross { Context::CrossSite } else { Context::Lax },
            partition: PartitionKey::of(Some(top), sfc_cross),
        }
    }

    /// The context of a WebSocket handshake from a document of `origin` (its site for cookies, as far as the socket
    /// knows it) to `target` (the handshake's `http(s):` URL).
    pub(crate) fn for_websocket(origin: Option<&url::Url>, target: &url::Url) -> HopContext {
        let cross = origin.is_some_and(|o| !crate::same_site(o, target));
        HopContext {
            get: if cross { Context::CrossSite } else { Context::Strict },
            set: if cross { Context::CrossSite } else { Context::Lax },
            partition: PartitionKey::of(Some(origin.unwrap_or(target)), cross),
        }
    }

    /// A document's response handed to the engine from outside (`Engine::load_page_delivered`): a top-level
    /// navigation's.
    fn top_level(target: &url::Url) -> HopContext {
        HopContext { get: Context::Strict, set: Context::Lax, partition: PartitionKey::of(Some(target), false) }
    }
}

/// The `Cookie` header field values for a hop to `url` (the shape wreq's cookie layer wants): one `name=value; ...`
/// line for HTTP/1.1, one field per cookie for HTTP/2 and HTTP/3 (the crumbs HPACK and QPACK split it into).
fn header_cookies(url: &url::Url, ctx: &HopContext, version: wreq::Version) -> wreq::cookie::Cookies {
    let cookies = shared().get_for_url(&CookieUrl::from_url(url), &Options { context: ctx.get, exclude_http_only: false }, ctx.partition.as_ref());
    if cookies.is_empty() {
        return wreq::cookie::Cookies::Empty;
    }
    if matches!(version, wreq::Version::HTTP_2 | wreq::Version::HTTP_3) {
        wreq::cookie::Cookies::Uncompressed(cookies.iter().filter_map(|(c, _)| wreq::header::HeaderValue::from_str(&c.line_entry()).ok()).collect())
    } else {
        match wreq::header::HeaderValue::from_str(&cookie_line(&cookies)) {
            Ok(v) => wreq::cookie::Cookies::Compressed(v),
            Err(_) => wreq::cookie::Cookies::Empty,
        }
    }
}

/// Store a response's `Set-Cookie` lines for its hop to `url` (`URLRequestHttpJob::SaveCookiesAndNotifyHeadersComplete`):
/// each a cookie created now, its `Expires` corrected by the response's `Date` (`server_time`), set in the hop's
/// context.
pub(crate) fn store_response<'a>(url: &url::Url, lines: impl Iterator<Item = &'a str>, server_time: Option<Micros>, ctx: &HopContext) {
    let source = CookieUrl::from_url(url);
    let opts = Options { context: ctx.set, exclude_http_only: false };
    for line in lines {
        if let Some(cc) = CanonicalCookie::create(&source, line, now(), server_time, ctx.partition.as_ref()) {
            // the `shared()` guard drops at this statement's end: its write-back is done and the jar's lock
            // released before the D9 cookie events go out (ABI 3, step 12)
            let changes = shared().set_canonical(cc, &source, &opts);
            if let Some(changes) = changes {
                note_cookie_changes(&changes);
            }
        }
    }
}

/// A `Date` header as the store's clock (`HttpResponseHeaders::GetDateValue`).
pub(crate) fn server_time(date: Option<&str>) -> Option<Micros> {
    let secs = crate::http_cache::parse_http_date(date?)?;
    Some((secs * MICROS_PER_SECOND as f64) as Micros)
}

/// The cookies a request hop sends; what it stores is [`store_response`]'s, called with the response's `Date`.
pub(crate) struct SendCookies(pub(crate) HopContext);

impl wreq::cookie::CookieStore for SendCookies {
    fn set_cookies(&self, _: &mut dyn Iterator<Item = &wreq::header::HeaderValue>, _: &wreq::Uri) {}

    fn cookies(&self, uri: &wreq::Uri, version: wreq::Version) -> wreq::cookie::Cookies {
        match url::Url::parse(&uri.to_string()) {
            Ok(u) => header_cookies(&u, &self.0, version),
            Err(_) => wreq::cookie::Cookies::Empty,
        }
    }
}

/// The cookies an exchange both sends and stores (a WebSocket handshake: one request, never redirected).
pub(crate) struct ExchangeCookies(pub(crate) HopContext);

impl wreq::cookie::CookieStore for ExchangeCookies {
    fn set_cookies(&self, cookie_headers: &mut dyn Iterator<Item = &wreq::header::HeaderValue>, uri: &wreq::Uri) {
        if let Ok(u) = url::Url::parse(&uri.to_string()) {
            store_response(&u, cookie_headers.filter_map(|v| v.to_str().ok()), None, &self.0);
        }
    }

    fn cookies(&self, uri: &wreq::Uri, version: wreq::Version) -> wreq::cookie::Cookies {
        match url::Url::parse(&uri.to_string()) {
            Ok(u) => header_cookies(&u, &self.0, version),
            Err(_) => wreq::cookie::Cookies::Empty,
        }
    }
}

/// A document's response the engine was HANDED (`Engine::load_page_delivered`) stores its cookies as the
/// navigation that fetched it would have.
pub fn store_delivered_response(url: &str, header: &str) {
    if let Ok(u) = url::Url::parse(url) {
        store_response(&u, std::iter::once(header), None, &HopContext::top_level(&u));
    }
}

// ── a document's script ──────────────────────────────────────────────────────────────────────────────

/// Where a document's script reaches the store from (`RestrictedCookieManager` for a frame): its cookie URL, whether
/// the document is in a cross-site context (some document from it up to its top-level one is not same-site with the
/// top-level one), and its partition.
#[derive(Clone, Debug)]
pub struct ScriptContext {
    url: CookieUrl,
    cross_site: bool,
    partition: Option<PartitionKey>,
}

impl ScriptContext {
    /// A document at `url` under the top-level document at `top_level` (`None`: it is the top-level one). `None` for a
    /// URL that does not parse.
    pub fn new(url: &str, cross_site: bool, top_level: Option<&str>) -> Option<ScriptContext> {
        let u = url::Url::parse(url).ok()?;
        if u.scheme() == "about" {
            return Some(ScriptContext { url: CookieUrl::harness(), cross_site: false, partition: None });
        }
        let top = top_level.and_then(|t| url::Url::parse(t).ok());
        let partition = PartitionKey::of(Some(top.as_ref().unwrap_or(&u)), cross_site);
        Some(ScriptContext { url: CookieUrl::from_url(&u), cross_site, partition })
    }

    /// The engine-from-a-string document, whose cookies are its realm's own (module docs).
    pub fn is_harness(&self) -> bool {
        self.url.harness
    }

    /// `ComputeSameSiteContextForScriptGet`.
    fn get_options(&self) -> Options {
        Options { context: if self.cross_site { Context::CrossSite } else { Context::Strict }, exclude_http_only: true }
    }

    /// `ComputeSameSiteContextForScriptSet`: a write needs only Lax.
    fn set_options(&self) -> Options {
        Options { context: if self.cross_site { Context::CrossSite } else { Context::Lax }, exclude_http_only: true }
    }

    /// The store this document's cookies are in: the process's, or a harness document's own `harness`.
    fn with_store<R>(&self, harness: &mut CookieMonster, f: impl FnOnce(&mut CookieMonster) -> R) -> R {
        if self.url.harness {
            f(harness)
        } else {
            let mut store = shared();
            f(&mut *store)
        }
    }
}

/// `document.cookie`'s getter: the cookies the document's script may read, as a cookie line.
pub fn document_cookie(ctx: &ScriptContext, harness: &mut CookieMonster) -> String {
    let opts = ctx.get_options();
    cookie_line(&ctx.with_store(harness, |s| s.get_for_url(&ctx.url, &opts, ctx.partition.as_ref())))
}

/// `document.cookie`'s setter (`RestrictedCookieManager::SetCookieFromString`): the changes the write made, empty when
/// it was refused.
pub fn set_document_cookie(ctx: &ScriptContext, line: &str, harness: &mut CookieMonster) -> Vec<Change> {
    let Some(cc) = CanonicalCookie::create(&ctx.url, line, now(), None, ctx.partition.as_ref()) else { return Vec::new() };
    let opts = ctx.set_options();
    let changes = ctx.with_store(harness, |s| s.set_canonical(cc, &ctx.url, &opts)).unwrap_or_default();
    note_cookie_changes(&changes); // ABI 3 (step 12, D9)
    changes
}

/// A cookie as `cookieStore` hands it to a page (Blink's `CookieChangeEvent::ToCookieListItem`). A deleted cookie
/// carries no `value` and no `expires`; a host-only cookie's `domain` is null; `same_site` is the effective one when
/// the access knew it, else the attribute's, absent when neither says.
#[derive(Clone, Debug, PartialEq)]
pub struct CookieItem {
    pub name: String,
    /// `None`: a deleted cookie (no `value` member).
    pub value: Option<String>,
    pub domain: Option<String>,
    pub path: String,
    /// `None`: no `expires` member (a deleted cookie); `Some(None)`: `null` (a session cookie); else milliseconds since
    /// the epoch.
    pub expires: Option<Option<f64>>,
    pub secure: bool,
    pub same_site: Option<&'static str>,
    pub partitioned: bool,
}

fn same_site_token(e: Effective) -> &'static str {
    match e {
        Effective::Strict => "strict",
        Effective::Lax | Effective::LaxAllowUnsafe => "lax",
        Effective::NoRestriction => "none",
    }
}

fn item_of(c: &CanonicalCookie, effective: Option<Effective>, deleted: bool) -> CookieItem {
    let same_site = effective.map(same_site_token).or(match c.same_site {
        SameSite::Strict => Some("strict"),
        SameSite::Lax => Some("lax"),
        SameSite::NoRestriction => Some("none"),
        SameSite::Unspecified => None,
    });
    CookieItem {
        name: c.name.clone(),
        value: (!deleted).then(|| c.value.clone()),
        domain: c.is_domain_cookie().then(|| c.domain[1..].to_string()),
        path: c.path.clone(),
        expires: (!deleted).then(|| c.expiry.map(|e| e as f64 / 1000.0)),
        secure: c.secure,
        same_site,
        partitioned: c.partition.is_some(),
    }
}

/// `cookieStore.get()` / `getAll()` (`RestrictedCookieManager::GetAllForUrl`): the cookies the document's script may
/// read, in cookie-line order; only those named `name` when given.
pub fn script_cookies(ctx: &ScriptContext, name: Option<&str>, harness: &mut CookieMonster) -> Vec<CookieItem> {
    let opts = ctx.get_options();
    let list = ctx.with_store(harness, |s| s.get_for_url(&ctx.url, &opts, ctx.partition.as_ref()));
    list.iter().filter(|(c, _)| name.is_none_or(|n| c.name == n)).map(|(c, e)| item_of(c, Some(*e), false)).collect()
}

/// What `cookieStore.set()` / `delete()` asks to store (Blink's `ToCookieParams`, already validated there): the
/// `.`-prefixed lower-case domain or `""`, the path, the expiry (`None`: a session cookie), the SameSite token.
pub struct CookieInit<'a> {
    pub name: &'a str,
    pub value: &'a str,
    pub domain: &'a str,
    pub path: &'a str,
    pub expires: Option<Micros>,
    pub same_site: &'a str,
    pub partitioned: bool,
}

/// Whether `ctx`'s document may name `domain` (`.domain`) in `cookieStore.set()` (Blink's `ToCookieParams` asks
/// `GetCookieDomainWithString` before the backend does).
pub fn domain_settable(ctx: &ScriptContext, domain: &str) -> bool {
    cookie_domain_with_string(&ctx.url, domain).is_some()
}

/// Whether `cookieStore` may write from `ctx`'s document (`network::IsUrlPotentiallyTrustworthy`, a local scheme
/// excluded).
pub fn script_may_set_secure(ctx: &ScriptContext) -> bool {
    ctx.url.access != Access::NonCryptographic
}

/// `cookieStore.set()` / `delete()`'s backend (`RestrictedCookieManager::SetCanonicalCookie` over
/// `CanonicalCookie::CreateSanitizedCookie`): always `Secure`, never `HttpOnly`. `None`: refused -- the promise rejects
/// with "Cookie was malformed and could not be stored".
pub fn script_set(ctx: &ScriptContext, init: &CookieInit, harness: &mut CookieMonster) -> Option<Vec<Change>> {
    let url = &ctx.url;
    let (name, value) = (init.name, init.value);
    if parse_token_string(name) != name || parse_value_string(value) != value || parse_value_string(init.path) != init.path {
        return None;
    }
    if !valid_name_value_pair(name, value) {
        return None;
    }
    let domain_attr = if parse_value_string(init.domain) == init.domain && valid_cookie_value(init.domain) && init.domain.len() <= MAX_ATTRIBUTE_VALUE {
        init.domain
    } else {
        return None;
    };
    if url.host.starts_with('.') {
        return None;
    }
    let domain = cookie_domain_with_string(url, domain_attr)?;
    // the path, canonicalized as a URL path is
    let path = canon_path(url, init.path);
    if !init.path.is_empty() && path != init.path {
        return None;
    }
    let path = url::Url::parse("http://h").ok().map(|mut u| {
        u.set_path(&path);
        u.path().to_string()
    })?;
    if path.len() > MAX_ATTRIBUTE_VALUE && !init.path.is_empty() {
        return None;
    }
    let prefix = prefix_of(name);
    if !prefix_valid(prefix, url, true, false, domain_attr, &path) || (name.is_empty() && has_hidden_prefix_name(value)) {
        return None;
    }
    // a partitioned cookie is in the document's partition
    let partition = if init.partitioned { ctx.partition.clone() } else { None };
    if partition.is_some() && url.access == Access::NonCryptographic {
        return None;
    }
    let creation = now();
    let cc = CanonicalCookie {
        name: name.to_string(),
        value: value.to_string(),
        domain,
        path,
        creation,
        expiry: validate_expiry(init.expires, creation),
        secure: true,
        http_only: false,
        same_site: same_site_of(init.same_site),
        partition,
        source_port: url.port,
        source_secure: url.access != Access::NonCryptographic,
    };
    // `RestrictedCookieManager`: no cookie on another domain, none from a host with a leading dot
    if !domain_match(&cc.domain, &url.host) {
        return None;
    }
    let opts = ctx.set_options();
    let changes = ctx.with_store(harness, |s| s.set_canonical(cc, url, &opts));
    if let Some(changes) = &changes {
        note_cookie_changes(changes); // ABI 3 (step 12, D9): cookieStore set + delete
    }
    changes
}

/// ABI 3 (step 12, D9): tell a cookie-watching plugin of each change a store write returned, the `op` mapped from the
/// change's cause EXACTLY as [`change_items`] maps it to a `cookieStore` event -- an insertion (or an overwrite that
/// changed only the creation date) is a `"set"`, an already-expired write's deletion a `"delete"`, and an overwrite
/// with nothing a page could see changed (the cookie an insertion replaced, a no-op rewrite) nothing. Each write site
/// calls this with the `Vec<Change>` its `set_canonical` returned, the jar's lock already released.
/// [`note_cookie`](crate::context_store::note_cookie) is itself gated on a cookie watcher, so a no-plugin run does
/// nothing (invariant I9/J2).
/// TODO(1021-verify): harness documents (`about:blank`, `Engine::load_html`) reach this too -- their changes come back
/// the same way -- and the `domain` passed is the stored `CanonicalCookie::domain` (a domain cookie's leading dot kept).
fn note_cookie_changes(changes: &[Change]) {
    for ch in changes {
        let op = match ch.cause {
            Cause::Inserted | Cause::InsertedNoValueChangeOverwrite => "set",
            Cause::ExpiredOverwrite => "delete",
            Cause::Overwrite | Cause::InsertedNoChangeOverwrite => continue,
        };
        crate::context_store::note_cookie(&ch.cookie.name, &ch.cookie.domain, op);
    }
}

/// The items `listener`'s `cookieStore` change event carries for `changes` (`CookieChangeEvent::ToEventInfo`, with
/// `RestrictedCookieManager::Listener`'s filter): `(changed, deleted)`, each only of cookies the listener's document
/// may read. An overwrite with nothing visible changed, and the cookie an insertion replaced, are no event.
pub fn change_items(changes: &[Change], listener: &ScriptContext) -> (Vec<CookieItem>, Vec<CookieItem>) {
    let opts = listener.get_options();
    let current = now();
    let (mut changed, mut deleted) = (Vec::new(), Vec::new());
    for ch in changes {
        if ch.cookie.partition.is_some() && ch.cookie.partition != listener.partition {
            continue;
        }
        let Some(effective) = include_for(&ch.cookie, &listener.url, &opts, current) else { continue };
        match ch.cause {
            Cause::Inserted | Cause::InsertedNoValueChangeOverwrite => changed.push(item_of(&ch.cookie, Some(effective), false)),
            Cause::ExpiredOverwrite => deleted.push(item_of(&ch.cookie, None, true)),
            Cause::Overwrite | Cause::InsertedNoChangeOverwrite => {}
        }
    }
    (changed, deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cu(s: &str) -> CookieUrl {
        CookieUrl::from_url(&url::Url::parse(s).unwrap())
    }

    /// `parsed_cookie_unittest.cc`'s shapes: a lone value is nameless, attributes are case-insensitive and the last
    /// wins, whitespace around a token or value is not part of it, a terminator or a control character refuses the
    /// line, an oversized attribute value is dropped and the cookie kept.
    #[test]
    fn the_line_parses_as_parsed_cookie_does() {
        let pc = ParsedCookie::parse("  a b  =  c d ; Path=/x ; PATH=/y; secure; SameSite=Lax").unwrap();
        assert_eq!((pc.name(), pc.value()), ("a b", "c d"));
        assert_eq!(pc.attr(pc.path), Some("/y"));
        assert!(pc.secure());
        assert_eq!(pc.same_site(), SameSite::Lax);
        let bare = ParsedCookie::parse("BBB").unwrap();
        assert_eq!((bare.name(), bare.value()), ("", "BBB"));
        let eq = ParsedCookie::parse("a=b=c").unwrap();
        assert_eq!(eq.value(), "b=c");
        assert!(ParsedCookie::parse("a=b\nc").is_none());
        assert!(ParsedCookie::parse("a=b\x01").is_none());
        assert!(ParsedCookie::parse("a\tb=c").is_none());
        assert!(ParsedCookie::parse("   ").is_none());
        assert!(ParsedCookie::parse("=").is_none());
        let long = ParsedCookie::parse(&format!("a=b; path=/{}", "x".repeat(1100))).unwrap();
        assert_eq!(long.attr(long.path), None);
        assert_eq!(same_site_of("bogus"), SameSite::Unspecified);
    }

    /// `GetCookieDomainWithString` with the registry rule: `ck.localhost` is a registrable domain (`localhost` an
    /// unknown top-level label), `localhost` and `appspot.com` are registries, an IP host may name only itself.
    #[test]
    fn a_domain_attribute_follows_the_registry() {
        let a = cu("http://a.ck.localhost/d/x");
        assert_eq!(cookie_domain_with_string(&a, ""), Some("a.ck.localhost".into()));
        assert_eq!(cookie_domain_with_string(&a, "ck.localhost"), Some(".ck.localhost".into()));
        assert_eq!(cookie_domain_with_string(&a, ".CK.Localhost"), Some(".ck.localhost".into()));
        assert_eq!(cookie_domain_with_string(&a, "a.ck.localhost"), Some(".a.ck.localhost".into()));
        assert_eq!(cookie_domain_with_string(&a, "localhost"), None);
        assert_eq!(cookie_domain_with_string(&a, "b.ck.localhost"), None);
        assert_eq!(cookie_domain_with_string(&a, "ck2.localhost"), None);
        assert_eq!(cookie_domain_with_string(&a, "ck%2elocalhost"), None);
        let g = cu("https://recaptcha-demo.appspot.com/");
        assert_eq!(cookie_domain_with_string(&g, "appspot.com"), None);
        assert_eq!(cookie_domain_with_string(&g, "recaptcha-demo.appspot.com"), Some(".recaptcha-demo.appspot.com".into()));
        let u = cu("https://ca-re-demo-setup.uw.r.appspot.com/");
        assert_eq!(cookie_domain_with_string(&u, "recaptcha-demo.appspot.com"), None);
        let co = cu("https://www.example.co.uk/");
        assert_eq!(cookie_domain_with_string(&co, "co.uk"), None);
        assert_eq!(cookie_domain_with_string(&co, "example.co.uk"), Some(".example.co.uk".into()));
        let ip = cu("http://127.0.0.1:8080/");
        assert_eq!(cookie_domain_with_string(&ip, "127.0.0.1"), Some("127.0.0.1".into()));
        assert_eq!(cookie_domain_with_string(&ip, ".127.0.0.1"), Some("127.0.0.1".into()));
        assert_eq!(cookie_domain_with_string(&ip, "0.0.1"), None);
        let lh = cu("http://localhost/");
        assert_eq!(cookie_domain_with_string(&lh, "localhost"), Some("localhost".into()));
        assert_eq!(cookie_domain_with_string(&lh, "127.0.0.1"), None);
    }

    /// `ParseCookieExpirationTime`: the parts in any order, two-digit years, a date that does not exist is none.
    #[test]
    fn expires_parses_as_chrome_reads_it() {
        let t = |s: &str| parse_cookie_expiration_time(s).map(|m| m / MICROS_PER_SECOND);
        assert_eq!(t("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(t("Sun, 06 Nov 1994 08:49:37 GMT"), Some(784_111_777));
        assert_eq!(t("Sunday, 06-Nov-94 08:49:37 GMT"), Some(784_111_777));
        assert_eq!(t("Sun Nov  6 08:49:37 1994"), Some(784_111_777));
        assert_eq!(t("1994 November 6 08:49:37"), Some(784_111_777));
        assert_eq!(t("Mon, 30 Feb 2099 00:00:00 GMT"), None);
        assert_eq!(t("Thu, 01 Jan 1970 00:00:60 GMT"), None);
        assert_eq!(t("Thu, 01 Jan 1970 GMT"), None);
        assert_eq!(string_to_i64("12abc"), (12, false));
        assert_eq!(string_to_i64("-5"), (-5, true));
        assert_eq!(string_to_i64("99999999999999999999"), (i64::MAX, false));
    }

    /// `ComputeSameSiteContextForRequest` / `ForResponse` and the partition, from what a request knows: a top-level
    /// navigation is strictly same-site from its own site or the user, Lax from another, Lax with an unsafe method
    /// for a cross-site POST, its own top-level site; a subresource cross-site to the page is the cross-site context
    /// and the page's cross-site partition.
    #[test]
    fn a_hop_meets_the_store_in_chromes_context() {
        let u = |s: &str| url::Url::parse(s).unwrap();
        let (page, other) = (u("https://a.example/"), u("https://b.test/x"));
        let mut nav = crate::FetchRequest::navigation(other.to_string());
        let c = HopContext::for_hop(&nav, None, None, &other, "GET", false);
        assert_eq!((c.get, c.set), (Context::Strict, Context::Lax));
        nav.initiator = Some(page.to_string());
        assert_eq!(HopContext::for_hop(&nav, Some(&page), None, &other, "GET", false).get, Context::Lax);
        let c = HopContext::for_hop(&nav, Some(&page), None, &other, "POST", false);
        assert_eq!(c.get, Context::LaxMethodUnsafe);
        assert_eq!(c.partition, Some(PartitionKey { top_site: "https://b.test".into(), cross_site: false }));
        let mut img = crate::FetchRequest::subresource(other.to_string(), "image", crate::Mode::NoCors, page.to_string());
        img.cookie_cross_site = true;
        let c = HopContext::for_hop(&img, Some(&page), Some(&page), &other, "GET", false);
        assert_eq!((c.get, c.set), (Context::CrossSite, Context::CrossSite));
        assert_eq!(c.partition, Some(PartitionKey { top_site: "https://a.example".into(), cross_site: true }));
        let own = u("https://a.example/i.png");
        let c = HopContext::for_hop(&crate::FetchRequest::subresource(own.to_string(), "image", crate::Mode::NoCors, page.to_string()), Some(&page), Some(&page), &own, "GET", false);
        assert_eq!((c.get, c.set), (Context::Strict, Context::Lax));
    }

    /// The store's own rules: an unchanged value keeps its creation date, a changed one moves to the end; a script
    /// cannot touch an HttpOnly cookie; an insecure origin leaves a Secure cookie alone; the line is ordered by path
    /// length, then age.
    #[test]
    fn the_store_orders_and_guards_as_cookie_monster() {
        let mut m = CookieMonster::default();
        let url = cu("http://a.ck.localhost/d/x");
        let script = Options { context: Context::Lax, exclude_http_only: true };
        let net = Options { context: Context::Lax, exclude_http_only: false };
        let get = Options { context: Context::Strict, exclude_http_only: true };
        // a millisecond apart, so no two creation dates tie
        let set = |m: &mut CookieMonster, line: &str, o: &Options| {
            std::thread::sleep(std::time::Duration::from_millis(1));
            let cc = CanonicalCookie::create(&url, line, now(), None, None)?;
            m.set_canonical(cc, &url, o)
        };
        for l in ["o1=1", "o2=1", "o3=1", "o1=1", "o2=2", "o3=1; path=/d/"] {
            assert!(set(&mut m, l, &script).is_some());
        }
        assert_eq!(cookie_line(&m.get_for_url(&url, &get, None)), "o3=1; o1=1; o3=1; o2=2");
        assert!(set(&mut m, "h=1; HttpOnly; path=/", &net).is_some());
        assert!(set(&mut m, "h=2; path=/", &script).is_none());
        assert!(set(&mut m, "x=1; HttpOnly", &script).is_none());
        let insecure = cu("http://example.com/");
        let cc = CanonicalCookie::create(&cu("https://example.com/"), "s=1; Secure", now(), None, None).unwrap();
        assert!(m.set_canonical(cc, &cu("https://example.com/"), &net).is_some());
        let cc = CanonicalCookie::create(&insecure, "s=2", now(), None, None).unwrap();
        assert!(m.set_canonical(cc, &insecure, &net).is_none());
    }
}
