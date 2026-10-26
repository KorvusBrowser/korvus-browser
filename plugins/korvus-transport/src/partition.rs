//! WHICH CONNECTION A REQUEST MAY RIDE: Chrome's connection partitioning.
//!
//! Chrome pools a connection under a GROUP (`net::ClientSocketPool::GroupId`, and an HTTP/2 session under
//! the `SpdySessionKey` built from the same parts): the destination, the request's PRIVACY MODE, and its
//! NETWORK ANONYMIZATION KEY -- the top-level site, and whether the frame that asked is cross-site to it. A
//! request reuses a connection of its own group and no other. This client pooled by destination alone, so
//! every request to an origin shared one connection, whichever document asked and whatever it may send.
//!
//! Measured on stock Chrome 150.0.7871.46 (`--log-net-log`, a rateyourmusic.com load through an HTTP
//! proxy, 2026-10-08), the groups by their `GroupId::ToString`:
//!
//! ```text
//! the page's api.js <script> (credentials omitted)   pm/https://challenge.example.com <https://rateyourmusic.com same_site>   new
//! the challenge <iframe>'s navigation                 https://challenge.example.com <https://rateyourmusic.com cross_site>      new
//! the frame's XHRs, its preload, its worker's fetch   (the frame's connection)                                                     reused
//! the worker's fetch to another host, no credentials  pm/https://worker.challenge.example.com <https://rateyourmusic.com cross_site>
//! ```
//!
//! The traced Chromium 150 agrees on the same flow: the frame's navigation waited 430 ms for its own
//! connection (CONNECT and TLS through the proxy) before its request went out. This engine sent it down
//! the page's `api.js` connection at once -- the frame's navigation entry read `requestStart` 0.08 ms where
//! Chrome's read 433 -- and every request the challenge made after rode a connection the page had opened,
//! which no Chrome does.
//!
//! The PRIVACY MODE (`URLRequestHttpJob::DeterminePrivacyMode`): a request whose credentials are not
//! included is `pm/` -- `omit`, and `same-origin` to another origin, which the CORS loader hands the network
//! as `omit`; it still offers client certificates while `kOmitCorsClientCert` is off, its default, so it is
//! never `pmwocc/`. A request with credentials is asked of the cookie settings: allowed is no prefix; a
//! third-party context under a browser that blocks third-party cookies keeps partitioned state only,
//! `pmpsa/`. The KEY (`NetworkAnonymizationKey::ToDebugString`): `<top-level site> same_site|cross_site`.

use crate::{FetchRequest, Mode};

/// The pool group of a request for `target` at this hop, spelled as Chrome's `GroupId::ToString` less the
/// destination (the client pools by destination already).
pub(crate) fn group(req: &FetchRequest, target: &url::Url, top_level: Option<&url::Url>, initiator: Option<&url::Url>, url_list: &[url::Url]) -> String {
    format!("{}<{}>", privacy_mode(req, initiator, url_list), anonymization_key(req, target, top_level, initiator))
}

/// A CORS preflight's group: Chrome sends the preflight with credentials mode `omit` (`PreflightController`),
/// so it is `pm/`, under the request's own key.
pub(crate) fn preflight_group(req: &FetchRequest, target: &url::Url, top_level: Option<&url::Url>, initiator: Option<&url::Url>) -> String {
    format!("pm/<{}>", anonymization_key(req, target, top_level, initiator))
}

fn privacy_mode(req: &FetchRequest, initiator: Option<&url::Url>, url_list: &[url::Url]) -> &'static str {
    if !crate::credentials_included(req, initiator, url_list) {
        return "pm/";
    }
    if req.third_party_cookies_blocked && req.cookie_cross_site {
        "pmpsa/"
    } else {
        ""
    }
}

/// The request's network anonymization key. A top-level navigation's frame IS its target. A nested
/// navigation's frame is the document it will load -- its target -- under the page's top-level site.
/// Anything else was asked for by a document, or by a dedicated worker, which takes its creator's key:
/// `initiator`'s site, under the top-level site.
fn anonymization_key(req: &FetchRequest, target: &url::Url, top_level: Option<&url::Url>, initiator: Option<&url::Url>) -> String {
    let (top, frame) = sites(req, target, top_level, initiator);
    format!("{top} {}", if top == frame { "same_site" } else { "cross_site" })
}

/// The HTTP cache's partition for this hop (`HttpCache::GenerateCacheKey`): its network ISOLATION key -- the
/// top-level site and the frame's own site, not only whether they differ -- with a nested document's own entries
/// kept apart (`s_`).
pub(crate) fn cache_partition(req: &FetchRequest, target: &url::Url, top_level: Option<&url::Url>, initiator: Option<&url::Url>) -> String {
    let (top, frame) = sites(req, target, top_level, initiator);
    let subframe_document = req.mode == Mode::Navigate && req.destination.token() != "document";
    format!("_dk_{}{top} {frame}", if subframe_document { "s_" } else { "" })
}

/// The top-level site and the frame's site of this hop's request.
fn sites(req: &FetchRequest, target: &url::Url, top_level: Option<&url::Url>, initiator: Option<&url::Url>) -> (String, String) {
    let target_site = site(target).unwrap_or_default();
    let top_navigation = req.mode == Mode::Navigate && req.destination.token() == "document";
    if top_navigation {
        (target_site.clone(), target_site)
    } else if req.mode == Mode::Navigate {
        let top = top_level.and_then(site).or_else(|| initiator.and_then(site)).unwrap_or_else(|| target_site.clone());
        (top, target_site)
    } else {
        let frame = initiator.and_then(site).or_else(|| top_level.and_then(site)).unwrap_or_else(|| target_site.clone());
        (top_level.and_then(site).unwrap_or_else(|| frame.clone()), frame)
    }
}

/// A URL's site (`crate::schemeful_site`); `None` for a URL with no host, whose document's site is its
/// creator's -- the caller falls back to that.
fn site(u: &url::Url) -> Option<String> {
    crate::schemeful_site(u)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CredentialsMode, PageRequest};

    fn u(s: &str) -> url::Url {
        url::Url::parse(s).unwrap()
    }

    /// The four groups stock Chrome 150's NetLog put the challenge's requests in (module docs), from the
    /// requests as this engine builds them.
    #[test]
    fn groups_are_chromes_for_the_challenge_flow() {
        let top = u("https://rateyourmusic.com/");
        let frame = u("https://challenge.example.com/challenge/widget/params?lang=auto");
        // the page's api.js: <script crossorigin> -- cors, credentials same-origin, another origin
        let api = u("https://challenge.example.com/challenge/v0/api.js?onload=x&render=explicit");
        let mut script = FetchRequest::subresource(api.to_string(), "script", Mode::Cors, top.to_string());
        script.credentials = CredentialsMode::SameOrigin;
        assert_eq!(group(&script, &api, Some(&top), Some(&top), std::slice::from_ref(&api)), "pm/<https://rateyourmusic.com same_site>");
        // the <iframe>'s navigation, initiated by the page
        let nav = FetchRequest::subresource(frame.to_string(), "iframe", Mode::Navigate, top.to_string());
        assert_eq!(group(&nav, &frame, Some(&top), Some(&top), std::slice::from_ref(&frame)), "<https://rateyourmusic.com cross_site>");
        // the frame's XHR POST (same-origin, credentials same-origin): the frame's connection
        let fo = u("https://challenge.example.com/challenge/fo/1:2:3");
        let mut xhr = FetchRequest::subresource(fo.to_string(), "", Mode::Cors, frame.to_string());
        xhr.page = Some(PageRequest { origin: frame.origin().ascii_serialization(), ..Default::default() });
        xhr.credentials = CredentialsMode::SameOrigin;
        assert_eq!(group(&xhr, &fo, Some(&top), Some(&frame), std::slice::from_ref(&fo)), "<https://rateyourmusic.com cross_site>");
        // its worker's fetch to another host (credentials same-origin -> omitted): privacy mode, same key
        let worker = u("https://challenge.example.com/");
        let bh = u("https://worker.challenge.example.com/challenge/i/x");
        let mut f = FetchRequest::subresource(bh.to_string(), "", Mode::Cors, worker.to_string());
        f.page = Some(PageRequest { origin: worker.origin().ascii_serialization(), ..Default::default() });
        f.credentials = CredentialsMode::SameOrigin;
        assert_eq!(group(&f, &bh, Some(&top), Some(&worker), std::slice::from_ref(&bh)), "pm/<https://rateyourmusic.com cross_site>");
        // a top-level navigation is its own top-level site, whatever page it came from
        let mut top_nav = FetchRequest::navigation(frame.to_string());
        top_nav.initiator = Some(top.to_string());
        assert_eq!(group(&top_nav, &frame, Some(&top), Some(&top), std::slice::from_ref(&frame)), "<https://example.com same_site>");
        // and a preflight goes out without credentials under the request's key
        assert_eq!(preflight_group(&xhr, &fo, Some(&top), Some(&frame)), "pm/<https://rateyourmusic.com cross_site>");
    }

    /// A browser that blocks third-party cookies keeps a cross-site context's credentialed request to
    /// partitioned state (`CookieSettings::PrivacySetting`): `pmpsa/`.
    #[test]
    fn a_blocked_third_party_context_is_partitioned_state_only() {
        let top = u("https://a.example/");
        let frame = u("https://b.test/frame");
        let mut nav = FetchRequest::subresource(frame.to_string(), "iframe", Mode::Navigate, top.to_string());
        nav.cookie_cross_site = true;
        nav.third_party_cookies_blocked = true;
        assert_eq!(group(&nav, &frame, Some(&top), Some(&top), std::slice::from_ref(&frame)), "pmpsa/<https://a.example cross_site>");
        nav.third_party_cookies_blocked = false;
        assert_eq!(group(&nav, &frame, Some(&top), Some(&top), std::slice::from_ref(&frame)), "<https://a.example cross_site>");
    }

    /// Sites are schemeful and registrable-domain wide; a host with no registrable domain is its own site.
    #[test]
    fn sites_are_schemeful_registrable_domains() {
        assert_eq!(site(&u("https://www.example.co.uk:8443/x")).as_deref(), Some("https://example.co.uk"));
        assert_eq!(site(&u("http://127.0.0.1:9000/")).as_deref(), Some("http://127.0.0.1"));
        assert_eq!(site(&u("about:blank")), None);
    }
}
