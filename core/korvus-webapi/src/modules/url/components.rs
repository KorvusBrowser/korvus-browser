//! `modules/url/components.rs` — the WHATWG URL component getters and setters, ONE copy.
//!
//! WAS TWO INDEPENDENT HAND-ROLLED COPIES of the same nine-member algorithm:
//!   * `modules/url/url.rs` — `URL.protocol`/`host`/`hostname`/`port`/`pathname`/`search`/`hash`/…
//!   * `core/html/hyperlink.rs` — the identical nine on `HTMLHyperlinkElementUtils`, i.e. `<a>` and
//!     `<area>`.
//! Neither ran the spec's algorithm. Both spelled the setters as convenience calls on `url::Url`
//! (`set_scheme`, `set_host`, `set_query`, `parse::<u16>()`), and every one of those is the WRONG
//! shape for a member of the *basic URL parser with a state override*:
//!
//!   * `search = "??lang=fr"` — `trim_start_matches('?')` strips BOTH; the spec removes A SINGLE
//!     leading `?`, so the query is `?lang=fr`.
//!   * `search = ""` / `hash = ""` — `is_empty() => None` REMOVED the component; only the setter's
//!     own empty-string branch does that, and `hash = "#"` must leave an EMPTY fragment behind.
//!   * `port = "8080stuff"` — `parse::<u16>()` rejects the whole value; the port state STOPS at the
//!     first non-digit and keeps `8080`.
//!   * `host = "a/b"` / `protocol = "http:x"` — `set_host`/`set_scheme` reject a value with trailing
//!     junk; host parsing ENDS at the first `/ ? # \` and the scheme state ends at the first `:`.
//!   * tab/newline — the basic URL parser removes every ASCII tab and newline from its input, so
//!     `protocol = "ht\thttp"` is `http`. `url.rs` stripped for three members, `hyperlink.rs` for
//!     none, and `username`/`password` must NOT strip (they are not parsed; they are percent-encoded).
//!
//! THE CORRECT IMPLEMENTATION ALREADY SHIPPED IN THE LOCKED DEPENDENCY. `url 2.5.8` carries
//! `src/quirks.rs`, which is these algorithms verbatim — it exists for exactly this caller (Servo
//! binds its `URL` to it). It was simply never wired. So this module is a thin, documented shim over
//! `url::quirks` plus the two things `quirks` leaves to its caller (tab/newline stripping, which
//! `Url::set_query` does internally but `set_path`/`set_fragment`/`set_scheme`/host parsing do not,
//! and the `port = ""` branch), and BOTH call sites name it. There is no third copy.

use url::quirks;

/// The basic URL parser's step 2: *remove all ASCII tab or newline from input*. Unconditional — it
/// is not guarded by the state override — so every member that reaches the parser gets it. The two
/// that do not are `username` and `password`, which the spec percent-encodes without parsing (WPT
/// `url-setters-stripping.any.js` says so in as many words: "These do no stripping").
fn strip_tab_newline(value: &str) -> String {
    value.chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect()
}

// ── getters ─────────────────────────────────────────────────────────────────────────────────────

pub(crate) fn protocol(u: &url::Url) -> String { quirks::protocol(u).to_string() }
pub(crate) fn username(u: &url::Url) -> String { quirks::username(u).to_string() }
pub(crate) fn password(u: &url::Url) -> String { quirks::password(u).to_string() }
pub(crate) fn host(u: &url::Url) -> String { quirks::host(u).to_string() }
pub(crate) fn hostname(u: &url::Url) -> String { quirks::hostname(u).to_string() }
pub(crate) fn port(u: &url::Url) -> String { quirks::port(u).to_string() }
/// A `filesystem:` URL's pathname is its OUTER path (`super::gurl`); every other URL's is the spec's.
pub(crate) fn pathname(u: &url::Url) -> String {
    if u.scheme() == "filesystem" {
        if let Some(p) = super::gurl::filesystem_pathname(u) {
            return p;
        }
    }
    quirks::pathname(u).to_string()
}
pub(crate) fn search(u: &url::Url) -> String { quirks::search(u).to_string() }
pub(crate) fn hash(u: &url::Url) -> String { quirks::hash(u).to_string() }
pub(crate) fn href(u: &url::Url) -> String { quirks::href(u).to_string() }

/// <https://url.spec.whatwg.org/#concept-url-origin>, and the ONE place it differs from
/// `Url::origin()`: a `blob:` URL's origin is its *path URL's* origin only when that path URL's
/// scheme is `http`, `https` or `file` — anything else is a NEW OPAQUE ORIGIN. `url`'s `origin.rs`
/// recurses into the inner URL whatever its scheme, so `blob:ftp://x/` answered `ftp://x` and
/// `blob:blob:https://x/` answered `https://x` where both must be `"null"`.
pub(crate) fn origin(u: &url::Url) -> String {
    if u.scheme() == "blob" {
        // The path URL parsed as Chrome parses it (`super::gurl`), and Blink's answer for the two kinds the spec
        // does not list: a registered standard scheme's tuple and `file:`'s `file://` (Chrome 150, measured:
        // `blob:chrome-extension://abc/x` is `chrome-extension://abc`, `blob:file:///x` is `file://`,
        // `blob:filesystem:http://e.com/temporary/a` "null"). The recursion ends: neither kind is a `blob:`.
        return match super::gurl::parse(u.path()) {
            Some(inner) if matches!(inner.scheme(), "http" | "https" | "file") || super::gurl::is_chrome_standard(inner.scheme()) => origin(&inner),
            _ => "null".to_string(),
        };
    }
    // Blink's `SecurityOrigin` where it is not the spec's: a `filesystem:` URL's inner origin, `file://`, a
    // registered standard scheme's tuple (`super::gurl`).
    if let Some(o) = super::gurl::origin(u) {
        return o;
    }
    u.origin().ascii_serialization()
}

// ── setters ─────────────────────────────────────────────────────────────────────────────────────
//
// `true` means "the record may have changed, write it back"; `false` means the algorithm returned
// failure and the URL is untouched. `<a>`/`<area>` use it to decide whether to re-serialise into the
// `href` content attribute; `URL` ignores it, because its record IS the object.

/// <https://url.spec.whatwg.org/#dom-url-protocol> — scheme start state as state override.
///
/// Blink's rule, not the spec's, for a `filesystem:` or registered standard URL (`super::gurl::set_protocol`).
///
/// And Blink's for every URL in one respect: the value is NOT stripped of tabs and newlines. `KURL::SetProtocol`
/// canonicalizes the part before the first `:` as it stands, a tab or newline in it is not a scheme code point,
/// and the URL is left untouched -- `https://x/` given `"\thttp"` or `"http\n"` stays `https:` in Chrome 150
/// (Chromium's own `url-setters-stripping.any-expected.txt`: those six subtests fail), where the spec strips
/// first and answers `http:`.
pub(crate) fn set_protocol(u: &mut url::Url, value: &str) -> bool {
    if let Some(r) = super::gurl::set_protocol(u, value) {
        return r;
    }
    if value.split(':').next().unwrap_or("").contains(['\t', '\n', '\r']) {
        return false;
    }
    quirks::set_protocol(u, value).is_ok()
}

/// <https://url.spec.whatwg.org/#dom-url-username> — percent-encode, no parse, no stripping.
pub(crate) fn set_username(u: &mut url::Url, value: &str) -> bool {
    if let Some(r) = super::gurl::set_component(u, "username", value) {
        return r;
    }
    quirks::set_username(u, value).is_ok()
}

/// <https://url.spec.whatwg.org/#dom-url-password> — likewise, and the empty string is *no*
/// password rather than an empty one.
pub(crate) fn set_password(u: &mut url::Url, value: &str) -> bool {
    if let Some(r) = super::gurl::set_component(u, "password", value) {
        return r;
    }
    quirks::set_password(u, value).is_ok()
}

/// <https://url.spec.whatwg.org/#dom-url-host> — host state, which consumes an optional `:port`
/// and ends at the first `/ ? # \`.
pub(crate) fn set_host(u: &mut url::Url, value: &str) -> bool {
    if let Some(r) = super::gurl::set_component(u, "host", &strip_tab_newline(value)) {
        return r;
    }
    quirks::set_host(u, &strip_tab_newline(value)).is_ok()
}

/// <https://url.spec.whatwg.org/#dom-url-hostname> — hostname state; a `:` is failure here.
pub(crate) fn set_hostname(u: &mut url::Url, value: &str) -> bool {
    if let Some(r) = super::gurl::set_component(u, "hostname", &strip_tab_newline(value)) {
        return r;
    }
    quirks::set_hostname(u, &strip_tab_newline(value)).is_ok()
}

/// <https://url.spec.whatwg.org/#dom-url-port> — port state, which STOPS at the first non-digit
/// instead of rejecting the value, and whose empty string clears the port.
pub(crate) fn set_port(u: &mut url::Url, value: &str) -> bool {
    if let Some(r) = super::gurl::set_component(u, "port", value) {
        return r;
    }
    // THE EMPTY TEST IS AGAINST THE VALUE AS GIVEN, BEFORE STRIPPING, and the order is observable:
    // `url.port = "\n\n\t\t"` is NOT the empty string, so it goes to the port state, whose input is
    // empty after step 2 removes the tab/newlines — and a port state with no digits is failure, so
    // the existing port SURVIVES. Testing after the strip cleared it instead (measured against
    // `setters_tests.json`, which has exactly that case).
    if value.is_empty() {
        // The spec's own branch: "If the given value is the empty string, then set url's port to
        // null."
        return u.set_port(None).is_ok();
    }
    let stripped = strip_tab_newline(value);
    if stripped.is_empty() {
        // And this is why the branch above cannot simply be moved below the strip: `url`'s
        // `parse_port` folds the empty-string case INTO the port state (`!has_any_digit && Setter &&
        // input.is_empty()` is not an error, and clears the port), so handing it a value that step 2
        // emptied would clear a port the spec keeps. The port state on an empty input reaches EOF
        // with an empty buffer, sets nothing, and returns.
        return false;
    }
    quirks::set_port(u, &stripped).is_ok()
}

/// <https://url.spec.whatwg.org/#dom-url-pathname> — path start state; a URL with an opaque path
/// is left alone.
pub(crate) fn set_pathname(u: &mut url::Url, value: &str) -> bool {
    if let Some(r) = super::gurl::set_component(u, "pathname", &strip_tab_newline(value)) {
        return r;
    }
    quirks::set_pathname(u, &strip_tab_newline(value));
    true
}

/// <https://url.spec.whatwg.org/#dom-url-search> — the empty string NULLS the query; otherwise a
/// SINGLE leading `?` is removed and the rest goes through the query state. `Url::set_query` strips
/// tab/newline itself, so this one hands `quirks` the raw value on purpose: the empty-string test is
/// made against the value BEFORE stripping.
pub(crate) fn set_search(u: &mut url::Url, value: &str) -> bool {
    if let Some(r) = super::gurl::set_component(u, "search", value) {
        return r;
    }
    quirks::set_search(u, value);
    true
}

/// <https://url.spec.whatwg.org/#dom-url-hash> — the empty string NULLS the fragment; otherwise a
/// SINGLE leading `#` is removed and the rest goes through the fragment state. Written out rather
/// than delegated because `Url::set_fragment` uses a no-trim input, so `hash = "te\tst"` would
/// percent-encode the tab as `%09` instead of dropping it.
pub(crate) fn set_hash(u: &mut url::Url, value: &str) -> bool {
    if let Some(r) = super::gurl::set_component(u, "hash", &strip_tab_newline(value)) {
        return r;
    }
    if value.is_empty() {
        u.set_fragment(None);
        return true;
    }
    let input = value.strip_prefix('#').unwrap_or(value);
    u.set_fragment(Some(&strip_tab_newline(input)));
    true
}

// ── the URL parser's BASE, which four call sites had backwards ──────────────────────────────────

/// Parse `href` with `base` AS THE PARSER'S BASE — <https://url.spec.whatwg.org/#concept-basic-url-parser>
/// is never invoked without one when a document is doing the parsing, and `Url::join` already covers
/// the absolute case (an input with its own scheme *and* authority ignores the base).
///
/// THE ORDER IS THE WHOLE POINT. `core/html/hyperlink.rs` and `core/dom/element.rs` both tried an
/// ABSOLUTE parse first and fell back to the base, and the two orders disagree for a href that names
/// a scheme but no authority — which the spec treats as RELATIVE. In a document based at
/// `http://example.org/foo/bar`, `<a href="http:foo.com">` is `http://example.org/foo/foo.com`;
/// absolute-first answered `http://foo.com/`. `modules/url/url.rs`'s `UrlBehavior::construct` (the
/// `URL` constructor) already had the order right, so the engine held two answers to one question.
///
/// The absolute parse survives only as the fallback for an UNPARSEABLE base — a document with no URL
/// of its own, where there is nothing to resolve against and `join` can never be reached.
///
/// Both parses are Chrome's (`super::gurl`): a `filesystem:` or registered standard URL resolves as Blink's
/// `KURL` does, every other one as the spec's parser.
pub(crate) fn resolve(base: &str, href: &str) -> Option<url::Url> {
    // A pure function of its two strings, read over and over for the same ones (`a.href` of a page's links, each
    // read parsing the base and the reference again with IDNA: ~700 ns where Chrome's is ~120). Remembered per
    // thread, a bounded table dropped whole when full.
    thread_local! {
        // base -> reference -> answer (looked up by `&str`, nothing made for a hit), and how many answers in all
        static RESOLVED: std::cell::RefCell<(std::collections::HashMap<Box<str>, std::collections::HashMap<Box<str>, Option<url::Url>>>, usize)> = Default::default();
    }
    const KEEP: usize = 2048;
    if let Some(hit) = RESOLVED.with(|m| m.borrow().0.get(base).and_then(|refs| refs.get(href)).cloned()) {
        return hit;
    }
    let out = super::gurl::parse(base).and_then(|b| super::gurl::join(&b, href)).or_else(|| super::gurl::parse(href));
    RESOLVED.with(|m| {
        let (map, n) = &mut *m.borrow_mut();
        if *n >= KEEP {
            map.clear();
            *n = 0;
        }
        map.entry(Box::from(base)).or_default().insert(Box::from(href), out.clone());
        *n += 1;
    });
    out
}
