//! `modules/url/gurl.rs` -- where Chrome's URL parser is not the WHATWG one.
//!
//! Blink's `KURL` -- what `URL`, `<a href>` and every reflected URL attribute parse with -- is Chromium's `url`
//! library (GURL's canonicalizer), and `url_util.cc`'s `DoCanonicalize` takes three schemes off the WHATWG path
//! the `url` crate implements:
//!
//! * **`filesystem:`** is a NESTED URL (`ParseFileSystemUrl`, `url_parse.cc:483-584`;
//!   `CanonicalizeFileSystemUrl`, `url_canon_filesystemurl.cc`): an inner standard URL whose path is one
//!   segment, the file system's type, then the file's own path, query and fragment. The inner URL must have a
//!   scheme, be `file` or a standard scheme other than `filesystem`, and have a type -- so `filesystem:///x`
//!   (Fingerprint's probe), `filesystem:`, `filesystem:mailto:x` and `filesystem:http://e.com/` are invalid,
//!   the inner URL loses its user info (and a `file` inner URL its host), and `pathname` is the outer path only:
//!   `new URL("filesystem:http://example.com/temporary/a.txt").pathname` is `/a.txt` and its `origin`
//!   `http://example.com` (Blink's `SecurityOrigin` is the inner URL's). The `url` crate read every one of them
//!   as an opaque-path URL: valid, `pathname` the whole inner URL, `origin` "null".
//! * **The schemes Chrome registers as STANDARD** (`url::AddStandardScheme(..., SCHEME_WITH_HOST)`): content's
//!   `devtools`, `chrome`, `chrome-untrusted`, `chrome-error` (`content/common/url_schemes.cc:61-64`) and the
//!   Chrome embedder's `chrome-extension`, `isolated-app`, `chrome-native`, `chrome-search`, `chrome-distiller`
//!   (`chrome/common/chrome_content_client.cc`, `kChromeStandardURLSchemes`, the Linux build). They are parsed
//!   as a special URL is -- host canonicalized (lowercased), backslashes and missing slashes tolerated, an
//!   empty path `/` -- with NO user info and NO port (`url_canon_stdurl.cc:30-90`: `SCHEME_WITH_HOST` writes
//!   neither, whatever the input spells), and their origin is the tuple `scheme://host`, except for the
//!   no-access schemes `chrome-error` and `chrome-native` (`url_schemes.cc:88`, `chrome_content_client.cc:241`),
//!   whose origin is opaque.
//! * **`file:`'s origin** serializes as `file://` (Blink `SecurityOrigin::ToString`), not "null".
//!
//! Measured on Google Chrome 150.0.7871.46, headed, no DevTools, three runs alike:
//! `core/korvus-runtime/tests/fixtures/url-schemes/`. What is NOT here: the five "opaque non-special" schemes
//! GURL still parses the pre-standard way (`android`, `drivefs`, `chromeos-steam`, `steam`, `materialized-view`,
//! `url_util.cc:117-133`: `android://Host/x` has an empty host and the pathname `//Host/x`), which an
//! opaque-path `url::Url` cannot represent.

/// The schemes Chrome registers as standard beyond the six special ones (see the module header).
pub(crate) const CHROME_STANDARD_SCHEMES: [&str; 9] =
    ["devtools", "chrome", "chrome-untrusted", "chrome-error", "chrome-extension", "isolated-app", "chrome-native", "chrome-search", "chrome-distiller"];

/// The registered standard schemes whose origin is opaque all the same (the no-access schemes).
const NO_ACCESS_SCHEMES: [&str; 2] = ["chrome-error", "chrome-native"];

/// The WHATWG special schemes, which GURL calls standard too.
const SPECIAL_SCHEMES: [&str; 6] = ["http", "https", "ws", "wss", "ftp", "file"];

/// Whether `scheme` is one Chrome parses as a standard URL with a host only (see the module header).
pub(crate) fn is_chrome_standard(scheme: &str) -> bool {
    CHROME_STANDARD_SCHEMES.contains(&scheme)
}

/// Whether a parsed URL is one this module answers for: a `filesystem:` URL or a registered standard one.
pub(crate) fn is_chrome_scheme(u: &url::Url) -> bool {
    u.scheme() == "filesystem" || is_chrome_standard(u.scheme())
}

/// The first two steps both parsers take: leading and trailing C0 controls and spaces go, and so does every
/// ASCII tab and newline (GURL `TrimUrl` + `RemoveUrlWhitespace`).
fn preprocess(input: &str) -> String {
    input.trim_matches(|c: char| c <= '\u{20}').chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect()
}

/// GURL's `ExtractScheme` over a preprocessed string: an ASCII letter, then letters, digits, `+`, `-` or `.`, up
/// to the first `:`, lowercased (`CanonicalizeScheme`).
fn scheme_of(s: &str) -> Option<String> {
    let colon = s.find(':')?;
    let sc = &s[..colon];
    let mut chars = sc.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    Some(sc.to_ascii_lowercase())
}

/// Everything after the scheme's `:`.
fn after_scheme(s: &str) -> &str {
    s.find(':').map_or("", |i| &s[i + 1..])
}

fn is_slash(c: char) -> bool {
    c == '/' || c == '\\'
}

/// `new URL(input)` as Chrome parses it: [`filesystem`] and the registered standard schemes here, every other
/// scheme the WHATWG parser's. `None` is "Invalid URL".
pub(crate) fn parse(input: &str) -> Option<url::Url> {
    let s = preprocess(input);
    match scheme_of(&s).as_deref() {
        Some("filesystem") => filesystem(&s),
        Some(sc) if is_chrome_standard(sc) => standard_with_host(sc, after_scheme(&s)),
        _ => url::Url::parse(input).ok(),
    }
}

/// `new URL(input, base)` as Chrome resolves it (GURL `ResolveRelative`): an input naming a scheme of its own
/// is absolute -- `filesystem:` always ("there's no equivalent to e.g. http:index.html",
/// `url_canon_relative.cc:200`), a standard scheme unless it is the base's with fewer than two slashes after
/// it -- and a relative one is resolved against a `filesystem:` base's OUTER path and a registered standard
/// base as against a special URL.
pub(crate) fn join(base: &url::Url, input: &str) -> Option<url::Url> {
    let s = preprocess(input);
    let scheme = scheme_of(&s);
    if let Some(sc) = scheme.as_deref() {
        let ours = sc == "filesystem" || is_chrome_standard(sc);
        let same_relative = is_chrome_standard(sc) && sc == base.scheme() && after_scheme(&s).chars().take_while(|&c| is_slash(c)).count() < 2;
        if same_relative {
            return join_relative(base, after_scheme(&s));
        }
        if ours {
            return parse(&s);
        }
        if is_chrome_scheme(base) {
            // another scheme against one of ours: absolute, by the WHATWG parser
            return url::Url::parse(input).ok();
        }
    }
    if is_chrome_scheme(base) {
        return join_relative(base, &s);
    }
    base.join(input).ok()
}

/// A relative `input` (no scheme of its own) against one of this module's bases.
fn join_relative(base: &url::Url, input: &str) -> Option<url::Url> {
    if base.scheme() == "filesystem" {
        // GURL's authority-relative resolution (`DoResolveRelativeHost`) writes the base's scheme before
        // `//host/...` and canonicalizes that as a standard URL: `filesystem://other/x`, host and path, origin
        // opaque (Chrome 150, measured, backslashes alike)
        if input.chars().take(2).filter(|&c| is_slash(c)).count() == 2 {
            let stand_in = url::Url::parse(&format!("http:{input}")).ok()?;
            return rebuild_standard("filesystem", &stand_in);
        }
        // everything else against the OUTER URL -- its path, query and fragment
        let (inner, outer) = filesystem_parts(base)?;
        let mut outer_url = outer;
        if let Some(q) = base.query() {
            outer_url.push('?');
            outer_url.push_str(q);
        }
        if let Some(f) = base.fragment() {
            outer_url.push('#');
            outer_url.push_str(f);
        }
        let stand_in = url::Url::parse(&format!("http://h{outer_url}")).ok()?;
        let joined = stand_in.join(input).ok()?;
        return filesystem(&format!("filesystem:{inner}{}", path_query_fragment(&joined)));
    }
    let stand_in = standard_stand_in(base)?;
    let joined = stand_in.join(input).ok()?;
    rebuild_standard(base.scheme(), &joined)
}

/// `path?query#fragment` of a special URL.
fn path_query_fragment(u: &url::Url) -> String {
    let mut out = u.path().to_string();
    if let Some(q) = u.query() {
        out.push('?');
        out.push_str(q);
    }
    if let Some(f) = u.fragment() {
        out.push('#');
        out.push_str(f);
    }
    out
}

// ── the registered standard schemes ────────────────────────────────────────────────────────────

/// A `SCHEME_WITH_HOST` URL from what follows its `scheme:`: past every slash and backslash, the authority up
/// to the first of `/ \ ? #`, its user info and port dropped (canonicalized by no one: GURL writes neither, so
/// `:xyz` and `:99999` are dropped as `:80` is), the host and the rest canonicalized as a special URL's.
fn standard_with_host(scheme: &str, rest: &str) -> Option<url::Url> {
    let body = rest.trim_start_matches(is_slash);
    let end = body.find(|c: char| is_slash(c) || c == '?' || c == '#').unwrap_or(body.len());
    let (authority, tail) = body.split_at(end);
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host.rfind(':') {
        Some(i) if !host[i..].contains(']') => &host[..i],
        _ => host,
    };
    let stand_in = url::Url::parse(&format!("http://{host}{tail}")).ok()?;
    rebuild_standard(scheme, &stand_in)
}

/// `stand_in`'s host, path, query and fragment under `scheme`. `None` without a host: a standard URL must have
/// one (`url_canon_stdurl.cc:63-65`).
fn rebuild_standard(scheme: &str, stand_in: &url::Url) -> Option<url::Url> {
    let host = stand_in.host_str().filter(|h| !h.is_empty())?;
    url::Url::parse(&format!("{scheme}://{host}{}", path_query_fragment(stand_in))).ok()
}

/// A registered standard URL as the special URL it is canonicalized like -- what a relative URL resolves
/// against and a setter edits.
fn standard_stand_in(u: &url::Url) -> Option<url::Url> {
    let host = u.host_str().filter(|h| !h.is_empty())?;
    url::Url::parse(&format!("http://{host}{}", path_query_fragment(u))).ok()
}

// ── filesystem: ────────────────────────────────────────────────────────────────────────────────

/// A `filesystem:` URL from a preprocessed string that starts with its scheme (see the module header).
fn filesystem(s: &str) -> Option<url::Url> {
    let rest = after_scheme(s);
    // The query and the fragment are the OUTER URL's: everything from the first `?` or `#` on.
    let q = rest.find(['?', '#']).unwrap_or(rest.len());
    let (nested, tail) = rest.split_at(q);
    let inner_scheme = scheme_of(nested)?;
    let after = after_scheme(nested);
    // Where the inner URL's path begins: after its authority -- past every slash for a standard URL; for `file`,
    // after a host only when exactly two slashes lead (`ParseFileURL`: `file://host/p`), the path itself from the
    // last of three or more (`file:///p`) and from the start for fewer (`file:/p`, `file:p`).
    let (authority, path) = if inner_scheme == "file" {
        let slashes = after.chars().take_while(|&c| is_slash(c)).count();
        if slashes == 2 {
            let body = &after[slashes..];
            let end = body.find(is_slash).unwrap_or(body.len());
            (&body[..end], &body[end..])
        } else if slashes > 2 {
            ("", &after[slashes - 1..])
        } else {
            ("", after)
        }
    } else {
        let body = after.trim_start_matches(is_slash);
        let end = body.find(is_slash).unwrap_or(body.len());
        (&body[..end], &body[end..])
    };
    // A `file` path with no leading slash (`file:temporary/a`) is canonicalized with one.
    if inner_scheme == "file" && !path.is_empty() && !path.starts_with(is_slash) {
        return filesystem_file(path, tail);
    }
    // The inner path keeps its first segment -- the type -- and the rest is the outer URL's path; no path at all
    // is no type, which is invalid.
    if !path.starts_with(is_slash) {
        return None;
    }
    let type_end = path[1..].find(is_slash).map_or(path.len(), |i| i + 1);
    let (kind, outer) = path.split_at(type_end);
    let inner = match inner_scheme.as_str() {
        "filesystem" => return None,
        "file" => url::Url::parse(&format!("file://{kind}")).ok()?,
        sc if SPECIAL_SCHEMES.contains(&sc) => {
            let mut u = url::Url::parse(&format!("{sc}://{authority}{kind}")).ok()?;
            // `SCHEME_WITH_HOST_AND_PORT`: the user info goes, the port stays
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u
        }
        sc if is_chrome_standard(sc) => standard_with_host(sc, &format!("//{authority}{kind}"))?,
        _ => return None,
    };
    filesystem_from(&inner, outer, tail)
}

/// [`filesystem`] for a `file` inner URL whose path does not start with a slash: GURL does not split such a path
/// (`ParseFileSystemUrl` returns before it), so the whole of it is the type, canonicalized with a leading slash, and
/// the outer path is empty -- `/` (Chrome 150: `filesystem:file:temporary/a` is `filesystem:file:///temporary/a/`).
fn filesystem_file(path: &str, tail: &str) -> Option<url::Url> {
    let inner = url::Url::parse(&format!("file:///{path}")).ok()?;
    filesystem_from(&inner, "", tail)
}

/// The URL `filesystem:<inner><outer><tail>` once each part is canonical: `inner` the inner URL with its type,
/// which must be more than its leading slash (`url_canon_filesystemurl.cc:72`), `outer` the file's path
/// (`/` when empty, canonicalized as a special URL's path) and `tail` the raw query and fragment.
fn filesystem_from(inner: &url::Url, outer: &str, tail: &str) -> Option<url::Url> {
    if inner.path().len() <= 1 {
        return None;
    }
    let file = url::Url::parse(&format!("http://h{outer}{tail}")).ok()?;
    let mut prefix = inner.as_str().to_string();
    // `file:` writes no host (`url_canon_filesystemurl.cc:52-56`)
    if inner.scheme() == "file" {
        prefix = format!("file://{}", inner.path());
    }
    url::Url::parse(&format!("filesystem:{prefix}{}", path_query_fragment(&file))).ok()
}

/// A canonical `filesystem:` URL split into its inner URL with the type (`http://e.com/temporary`) and its outer
/// path (`/a.txt`). The `url` crate holds it as an opaque path, `http://e.com/temporary/a.txt`.
fn filesystem_parts(u: &url::Url) -> Option<(String, String)> {
    let p = u.path();
    let colon = p.find(':')?;
    let after = p[colon + 1..].strip_prefix("//")?;
    let path_start = colon + 1 + 2 + after.find('/')?;
    let path = &p[path_start..];
    let type_end = path[1..].find('/').map_or(path.len(), |i| i + 1);
    Some((p[..path_start + type_end].to_string(), p[path_start + type_end..].to_string()))
}

/// The outer path of a `filesystem:` URL -- what `pathname` answers.
pub(crate) fn filesystem_pathname(u: &url::Url) -> Option<String> {
    filesystem_parts(u).map(|(_, outer)| outer)
}

/// The origin of one of this module's URLs, or of a `file:` one; `None` for any other.
pub(crate) fn origin(u: &url::Url) -> Option<String> {
    match u.scheme() {
        // Blink's `SecurityOrigin` of a filesystem URL is its inner URL's
        "filesystem" => Some(
            filesystem_parts(u)
                .and_then(|(inner, _)| parse(&inner))
                .map(|inner| super::components::origin(&inner))
                .unwrap_or_else(|| "null".to_string()),
        ),
        "file" => Some("file://".to_string()),
        sc if NO_ACCESS_SCHEMES.contains(&sc) => Some("null".to_string()),
        sc if is_chrome_standard(sc) => Some(format!("{sc}://{}", u.host_str().unwrap_or(""))),
        _ => None,
    }
}

// ── setters ─────────────────────────────────────────────────────────────────────────────────────

/// Blink `KURL::SetProtocol` (`kurl.cc:477-546`): the value up to its first `:` is the new scheme -- NOT stripped
/// of whitespace ("Firefox and Safari don't do it"), refused when `CanonicalizeScheme` refuses it; a SPECIAL URL
/// may only become another special one (the WHATWG rule, `SchemeRegistry::IsSpecialScheme`), but ANY OTHER may
/// become anything: the scheme is replaced and the URL canonicalized again under the new one, as GURL's
/// `ReplaceComponents` does. Chrome 150, measured: `foo://x/y` with `protocol = "http"` is `http://x/y`,
/// `mailto:a@b` is `http://a@b/`, `data:x` with `"chrome"` is `chrome://x/`, `filesystem:http://e.com/temporary/a`
/// with `"https"` is `https://http//e.com/temporary/a` -- the WHATWG setter left all four unchanged. `Some(changed)`
/// for a non-special URL, `None` for a special one (the WHATWG setter applies).
pub(crate) fn set_protocol(u: &mut url::Url, value: &str) -> Option<bool> {
    if SPECIAL_SCHEMES.contains(&u.scheme()) {
        return None;
    }
    let new = value.split(':').next().unwrap_or("");
    let valid = new.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) && new.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !valid {
        return Some(false);
    }
    let rest = after_scheme(u.as_str()).to_string();
    match parse(&format!("{}:{rest}", new.to_ascii_lowercase())) {
        Some(n) => {
            *u = n;
            Some(true)
        }
        None => Some(false),
    }
}

/// The host, hostname, pathname, search and hash setters on one of this module's URLs; `None` when the WHATWG
/// setter applies. A registered standard URL takes them as the special URL it is canonicalized like, and has no
/// user info or port to set (`SCHEME_WITH_HOST`); a `filesystem:` URL's pathname is its OUTER path, and it has
/// no host, port or user info to set.
pub(crate) fn set_component(u: &mut url::Url, member: &str, value: &str) -> Option<bool> {
    if u.scheme() == "filesystem" {
        return Some(match member {
            "pathname" => {
                let Some((inner, _)) = filesystem_parts(u) else { return Some(false) };
                let Ok(mut stand_in) = url::Url::parse("http://h/") else { return Some(false) };
                url::quirks::set_pathname(&mut stand_in, value);
                let mut tail = String::new();
                if let Some(q) = u.query() {
                    tail.push('?');
                    tail.push_str(q);
                }
                if let Some(f) = u.fragment() {
                    tail.push('#');
                    tail.push_str(f);
                }
                match filesystem(&format!("filesystem:{inner}{}{tail}", stand_in.path())) {
                    Some(n) => {
                        *u = n;
                        true
                    }
                    None => false,
                }
            }
            // the outer URL's own two, which the opaque-path record sets as Chrome does
            "search" | "hash" => return None,
            _ => false,
        });
    }
    if !is_chrome_standard(u.scheme()) {
        return None;
    }
    Some(match member {
        "host" | "hostname" | "pathname" | "search" | "hash" => {
            let Some(mut stand_in) = standard_stand_in(u) else { return Some(false) };
            match member {
                "host" => {
                    // the host state stops at a `:`; the port it would read is not this URL's to have
                    let host = value.split(':').next().unwrap_or("");
                    let _ = url::quirks::set_hostname(&mut stand_in, host);
                }
                "hostname" => {
                    let _ = url::quirks::set_hostname(&mut stand_in, value);
                }
                "pathname" => url::quirks::set_pathname(&mut stand_in, value),
                "search" => url::quirks::set_search(&mut stand_in, value),
                _ => {
                    if value.is_empty() {
                        stand_in.set_fragment(None);
                    } else {
                        stand_in.set_fragment(Some(value.strip_prefix('#').unwrap_or(value)));
                    }
                }
            }
            match rebuild_standard(u.scheme(), &stand_in) {
                Some(n) => {
                    *u = n;
                    true
                }
                None => false,
            }
        }
        // `username`, `password`, `port`: a `SCHEME_WITH_HOST` URL has none
        _ => false,
    })
}
