//! HTTP Client Hints -- `Accept-CH`, `Critical-CH` and the high-entropy User-Agent hints they turn on.
//!
//! a bot challenge page answers `Accept-CH` and `Critical-CH` naming the high-entropy UA hints;
//! Chrome restarts the navigation once with them (`sec-ch-ua-full-version-list`, `-arch`, `-bitness`, ...,
//! and the cookie the first answer set), the restart DevTools shows as `307 Internal Redirect`, and
//! sends them on every later request to that origin. korvus did neither: its challenge ran on a "Chrome"
//! that ignored `Critical-CH`.
//!
//! As Chromium 150 does it (`content/browser/client_hints/`, `services/network/public/cpp/client_hints.cc`,
//! Blink `FrameFetchContext::AddClientHintsIfNecessary`), measured against Chrome 150.0.7871.46 on a
//! loopback fixture (`core/korvus-runtime/tests/fixtures/client-hints/`) and on a live challenge:
//!
//! * `Accept-CH` / `Critical-CH` are a structured-field list of bare tokens -- any other member voids the
//!   header -- matched case-insensitively against Chrome's hint names, unknown names skipped.
//! * Only a top-level navigation's response, from a potentially trustworthy origin, PERSISTS its origin's
//!   `Accept-CH` (replacing the last set); a frame's never does.
//! * A request carries the top-level origin's accepted hints when it goes to that origin from a document
//!   of that origin: the high-entropy hints' Permissions Policy default is `self`, so a cross-origin
//!   frame, and any request to another origin, keeps the low-entropy three only.
//! * `Critical-CH` restarts a top-level navigation -- once per origin -- when a hint it names that
//!   `Accept-CH` turned on went out missing; the restart is sent from the navigation's first URL.
//! * Wire order: a navigation's hints sit in the client-hint slot in Chromium's navigation order; a
//!   restart re-appends them after `Accept`; a subresource's go into Blink's header map in Blink's
//!   insertion order ([`crate::blink_headers`]).
//!
//! What is sent is the eleven User-Agent hints, their values the persona's own (`getHighEntropyValues`).
//! The other hints Chrome knows -- `Sec-CH-DPR`, `-Viewport-Width/Height`, `-Device-Memory`, `RTT`,
//! `Downlink`, `ECT`, `Sec-CH-Prefers-*`, `Save-Data` -- are parsed and persisted but not yet sent, and a
//! page's `<meta http-equiv="accept-ch">`, `delegate-ch` and `allow="ch-ua-*"` delegation are not read.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Chromium's client hint names, `services/network/public/cpp/client_hints.cc` `MakeClientHintToNameMap`.
pub(crate) const NAMES: [&str; 27] = [
    "device-memory",
    "dpr",
    "width",
    "viewport-width",
    "rtt",
    "downlink",
    "ect",
    "sec-ch-ua",
    "sec-ch-ua-arch",
    "sec-ch-ua-platform",
    "sec-ch-ua-model",
    "sec-ch-ua-mobile",
    "sec-ch-ua-full-version",
    "sec-ch-ua-platform-version",
    "sec-ch-prefers-color-scheme",
    "sec-ch-ua-bitness",
    "sec-ch-viewport-height",
    "sec-ch-device-memory",
    "sec-ch-dpr",
    "sec-ch-width",
    "sec-ch-viewport-width",
    "sec-ch-ua-full-version-list",
    "sec-ch-ua-wow64",
    "save-data",
    "sec-ch-prefers-reduced-motion",
    "sec-ch-ua-form-factors",
    "sec-ch-prefers-reduced-transparency",
];

/// The low-entropy three every request carries (the persona's identity headers state them).
pub(crate) const LOW_ENTROPY: [&str; 3] = ["sec-ch-ua", "sec-ch-ua-mobile", "sec-ch-ua-platform"];

/// The User-Agent hints in the order a NAVIGATION adds them (`UpdateNavigationRequestClientUaHeadersImpl`).
pub(crate) const UA_NAVIGATION_ORDER: [&str; 11] = [
    "sec-ch-ua",
    "sec-ch-ua-mobile",
    "sec-ch-ua-full-version",
    "sec-ch-ua-arch",
    "sec-ch-ua-platform",
    "sec-ch-ua-platform-version",
    "sec-ch-ua-model",
    "sec-ch-ua-bitness",
    "sec-ch-ua-wow64",
    "sec-ch-ua-full-version-list",
    "sec-ch-ua-form-factors",
];

/// The same hints in the order BLINK inserts them into a subresource's header map
/// (`FrameFetchContext::AddClientHintsIfNecessary`) -- where they land is the map's hash order.
pub(crate) const UA_BLINK_ORDER: [&str; 11] = [
    "sec-ch-ua",
    "sec-ch-ua-mobile",
    "sec-ch-ua-arch",
    "sec-ch-ua-platform",
    "sec-ch-ua-platform-version",
    "sec-ch-ua-model",
    "sec-ch-ua-full-version",
    "sec-ch-ua-full-version-list",
    "sec-ch-ua-bitness",
    "sec-ch-ua-wow64",
    "sec-ch-ua-form-factors",
];

/// A structured-field string (RFC 8941 sf-string), as `net::structured_headers::SerializeItem` writes it.
pub(crate) fn sf_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The persona's high-entropy UA hints as header values, from its `getHighEntropyValues` capture.
pub(crate) fn high_entropy_values(ch: &spec::ClientHints) -> Vec<(String, String)> {
    let list = |bs: &[spec::Brand]| {
        bs.iter().map(|b| format!("{};v={}", sf_string(&b.brand), sf_string(&b.version))).collect::<Vec<_>>().join(", ")
    };
    let mut out = vec![
        ("sec-ch-ua-full-version".to_string(), sf_string(&ch.ua_full_version)),
        ("sec-ch-ua-arch".to_string(), sf_string(&ch.architecture)),
        ("sec-ch-ua-platform-version".to_string(), sf_string(&ch.platform_version)),
        ("sec-ch-ua-model".to_string(), sf_string(&ch.model)),
        ("sec-ch-ua-bitness".to_string(), sf_string(&ch.bitness)),
        ("sec-ch-ua-wow64".to_string(), if ch.wow64 { "?1".to_string() } else { "?0".to_string() }),
    ];
    if !ch.full_version_list.is_empty() {
        out.push(("sec-ch-ua-full-version-list".to_string(), list(&ch.full_version_list)));
    }
    if !ch.form_factors.is_empty() {
        let ff = ch.form_factors.iter().map(|f| sf_string(f)).collect::<Vec<_>>().join(", ");
        out.push(("sec-ch-ua-form-factors".to_string(), ff));
    }
    out
}

fn is_token(t: &str) -> bool {
    let mut cs = t.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '*')
        && cs.all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~:/".contains(c))
}

/// `Accept-CH` / `Critical-CH`: the hint names a structured-field list of tokens names, in Chrome's
/// spelling; `None` when the value is not such a list (Chromium ignores the whole header then).
pub(crate) fn parse(value: &str) -> Option<Vec<&'static str>> {
    let v = value.trim_matches(|c| c == ' ' || c == '\t');
    let mut out = Vec::new();
    if v.is_empty() {
        return Some(out);
    }
    for member in v.split(',') {
        let m = member.trim_matches(|c| c == ' ' || c == '\t');
        // a token, optionally with parameters (`;key` or `;key=value`); an inner list or a string is not one
        let bare = m.split(';').next().unwrap_or("");
        if m.is_empty() || !is_token(bare) {
            return None;
        }
        if let Some(n) = NAMES.iter().find(|n| n.eq_ignore_ascii_case(bare)) {
            if !out.contains(n) {
                out.push(*n);
            }
        }
    }
    Some(out)
}

/// A response header's field values, combined as one list (`a, b` from two `Accept-CH` lines).
pub(crate) fn combined(headers: &[(String, String)], name: &str) -> Option<String> {
    let vs: Vec<&str> = headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).collect();
    (!vs.is_empty()).then(|| vs.join(", "))
}

fn store() -> &'static Mutex<HashMap<String, Vec<&'static str>>> {
    static S: OnceLock<Mutex<HashMap<String, Vec<&'static str>>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The hints `origin` accepted on its last top-level navigation response.
pub(crate) fn accepted(origin: &url::Origin) -> Vec<&'static str> {
    store().lock().ok().and_then(|s| s.get(&origin.ascii_serialization()).cloned()).unwrap_or_default()
}

/// Persist `origin`'s `Accept-CH` (a later one replaces it).
pub(crate) fn persist(origin: &url::Origin, hints: &[&'static str]) {
    if let Ok(mut s) = store().lock() {
        s.insert(origin.ascii_serialization(), hints.to_vec());
    }
}

/// The hints beyond the low-entropy three this request carries: the top-level origin's accepted set, for
/// a request to that origin from a document of that origin (a top-level navigation is its own top), over
/// a potentially trustworthy origin, in [`UA_NAVIGATION_ORDER`].
pub(crate) fn extra_for(target: &url::Url, top: Option<&url::Url>, initiator: Option<&url::Url>, top_navigation: bool) -> Vec<&'static str> {
    let top = if top_navigation { Some(target) } else { top.or(initiator) };
    let Some(top) = top else { return Vec::new() };
    let origin = top.origin();
    if !origin.is_tuple() || !crate::is_potentially_trustworthy(top) || target.origin() != origin {
        return Vec::new();
    }
    if !top_navigation && initiator.is_some_and(|i| i.origin() != origin) {
        return Vec::new();
    }
    let on = accepted(&origin);
    UA_NAVIGATION_ORDER.iter().copied().filter(|h| !LOW_ENTROPY.contains(h) && on.contains(h)).collect()
}

/// Does a top-level navigation's response at `url` restart the navigation (`CriticalClientHintsThrottle`)?
/// Its `Accept-CH` turned on a hint its `Critical-CH` names, and that hint was not on the request (`sent`);
/// at most once per origin per navigation (`restarted`).
pub(crate) fn critical_restart(url: &url::Url, accept: &[&'static str], critical: Option<&str>, sent: &[&str], restarted: &mut Vec<String>) -> bool {
    let Some(critical) = critical.and_then(parse) else { return false };
    let origin = url.origin().ascii_serialization();
    if restarted.contains(&origin) || !crate::is_potentially_trustworthy(url) {
        return false;
    }
    let missing = critical.iter().any(|h| accept.contains(h) && !sent.iter().any(|s| s.eq_ignore_ascii_case(h)));
    if missing {
        restarted.push(origin);
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_ch_parses_as_chromium_does() {
        assert_eq!(
            parse("Sec-CH-UA-Bitness, Sec-CH-UA-Arch, UA-Arch, sec-ch-ua-model"),
            Some(vec!["sec-ch-ua-bitness", "sec-ch-ua-arch", "sec-ch-ua-model"])
        );
        assert_eq!(parse(""), Some(vec![]));
        assert_eq!(parse("DPR;x=1, Sec-CH-UA-WoW64"), Some(vec!["dpr", "sec-ch-ua-wow64"]));
        // a string, an inner list, or an empty member voids the whole header
        assert_eq!(parse("\"Sec-CH-UA-Arch\""), None);
        assert_eq!(parse("(Sec-CH-UA-Arch)"), None);
        assert_eq!(parse("Sec-CH-UA-Arch,,DPR"), None);
        assert_eq!(parse("1dpr"), None);
    }

    #[test]
    fn sf_strings_escape() {
        assert_eq!(sf_string("x86"), "\"x86\"");
        assert_eq!(sf_string(""), "\"\"");
        assert_eq!(sf_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    #[test]
    fn a_request_carries_its_top_level_origins_hints_only_within_that_origin() {
        let top = url::Url::parse("https://ch-a.example/").unwrap();
        persist(&top.origin(), &["sec-ch-ua-arch", "sec-ch-ua-full-version-list", "dpr"]);
        let same = url::Url::parse("https://ch-a.example/s.js").unwrap();
        let other = url::Url::parse("https://cdn.example/s.js").unwrap();
        let want = vec!["sec-ch-ua-arch", "sec-ch-ua-full-version-list"];
        assert_eq!(extra_for(&top, None, None, true), want);
        assert_eq!(extra_for(&same, Some(&top), Some(&top), false), want);
        assert!(extra_for(&other, Some(&top), Some(&top), false).is_empty());
        // a cross-origin frame's own request back to the top-level origin
        assert!(extra_for(&same, Some(&top), Some(&other), false).is_empty());
        // plain http to a named host is not potentially trustworthy
        let http = url::Url::parse("http://ch-a.example/").unwrap();
        assert!(extra_for(&http, None, None, true).is_empty());
    }

    #[test]
    fn critical_ch_restarts_once_for_a_missing_accepted_hint() {
        let u = url::Url::parse("https://ch-b.example/").unwrap();
        let accept = parse("Sec-CH-UA-Arch, Sec-CH-UA-Model").unwrap();
        let mut done = Vec::new();
        // named but not accepted: no restart
        assert!(!critical_restart(&u, &accept, Some("Sec-CH-UA-Bitness"), &["sec-ch-ua"], &mut done));
        assert!(critical_restart(&u, &accept, Some("Sec-CH-UA-Arch"), &["sec-ch-ua"], &mut done));
        // once per origin
        assert!(!critical_restart(&u, &accept, Some("Sec-CH-UA-Arch"), &["sec-ch-ua"], &mut done));
        // already sent
        let mut fresh = Vec::new();
        assert!(!critical_restart(&u, &accept, Some("Sec-CH-UA-Arch"), &["sec-ch-ua-arch"], &mut fresh));
    }
}
