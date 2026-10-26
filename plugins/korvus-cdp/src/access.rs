//! Who may reach a CDP endpoint from a web page (D88 step 1).
//!
//! A browser lets any site open a WebSocket to `ws://127.0.0.1:9222`, so an endpoint that answers every handshake
//! hands any page the user visits the whole browser -- its pages, its cookies, a persona's saved sign-ins.
//! code-server shipped exactly that (CVE-2023-26114). Chrome closes it in two checks, and korvus makes the same two:
//!
//! * **`Origin`.** A browser stamps every WebSocket handshake with the page's origin and a page cannot forge it;
//!   a CDP library (Playwright, Puppeteer, websocket-client with `suppress_origin`) sends none. A handshake that
//!   carries one is refused with 403 unless it was allowed (`korvus serve --allow-origin <origin>`, `*` for all),
//!   as Chrome refuses one since 111 unless `--remote-allow-origins` names it -- `devtools://devtools` too, as
//!   Chrome 150 refuses it (measured, `tools/frontend/channels_probe.py --chrome`).
//! * **`Host`.** DNS rebinding points a site's own name at 127.0.0.1, after which its pages are same-origin with
//!   the endpoint and read `/json` (every open page's URL and title) without asking. Their requests still carry
//!   the site's name in `Host`, so a request whose `Host` is neither an IP nor `localhost` is refused with 500, as
//!   Chrome's DevTools handler refuses it. A name the endpoint really is reached by -- a reverse proxy's -- is
//!   allowed with `--allow-host <name>`.

use std::sync::OnceLock;

/// The browser endpoint's allowances (`korvus serve`), set once before it listens.
static ALLOWED: OnceLock<Allowed> = OnceLock::new();

/// The origins and host names an endpoint lets in beyond the defaults.
#[derive(Clone, Debug, Default)]
pub struct Allowed {
    /// Origins, as `scheme://host[:port]` (lower-cased); `*` lets every origin in.
    pub origins: Vec<String>,
    /// Host names (without a port) a request may name in `Host` besides IPs and `localhost`.
    pub hosts: Vec<String>,
}

/// Set the browser endpoint's allowances (`--allow-origin`, `--allow-host`). The first call wins.
pub fn allow(a: Allowed) {
    let a = Allowed {
        origins: a.origins.iter().map(|o| o.trim().trim_end_matches('/').to_ascii_lowercase()).filter(|o| !o.is_empty()).collect(),
        hosts: a.hosts.iter().map(|h| h.trim().to_ascii_lowercase()).filter(|h| !h.is_empty()).collect(),
    };
    let _ = ALLOWED.set(a);
}

fn allowed() -> &'static Allowed {
    ALLOWED.get_or_init(Allowed::default)
}

/// Why a request was turned away, and the answer it gets.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    Host,
    Origin(String),
}

impl Refusal {
    /// The HTTP response that refuses the request: Chrome's status for each, with a line that says which flag
    /// lets it in.
    pub fn response(&self) -> String {
        let (status, body) = match self {
            Refusal::Host => ("500 Internal Server Error", "Host header is specified and is not an IP address or localhost.".to_string()),
            Refusal::Origin(o) => (
                "403 Forbidden",
                format!(
                    "Rejected an incoming WebSocket connection from the {o} origin. Use `korvus serve --allow-origin {o}` to allow connections from this origin or `--allow-origin '*'` to allow all origins."
                ),
            ),
        };
        format!("HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    /// The line the endpoint logs for it.
    pub fn log_line(&self) -> String {
        match self {
            Refusal::Host => "refused a request whose Host is neither an IP nor localhost (`--allow-host` names one)".into(),
            Refusal::Origin(o) => format!("refused a WebSocket from the {o} origin (`--allow-origin {o}` lets it in)"),
        }
    }
}

/// Whether the browser endpoint serves a request with these `Host` and `Origin` headers: `websocket` for a
/// handshake (the only kind `Origin` is checked on, as Chrome checks it).
pub fn check_browser(host: Option<&str>, origin: Option<&str>, websocket: bool) -> Result<(), Refusal> {
    check(host, origin, websocket, allowed())
}

/// The same for a page process's own socket, which only its router opens: no origin is let in there, whatever the
/// browser endpoint allows, since a site that finds the page's loopback port must not reach around the router.
pub fn check_page(host: Option<&str>, origin: Option<&str>, websocket: bool) -> Result<(), Refusal> {
    check(host, origin, websocket, &Allowed::default())
}

fn check(host: Option<&str>, origin: Option<&str>, websocket: bool, a: &Allowed) -> Result<(), Refusal> {
    if let Some(h) = host.filter(|h| !h.is_empty()) {
        if !host_ok(h, &a.hosts) {
            return Err(Refusal::Host);
        }
    }
    if websocket {
        if let Some(o) = origin.map(str::trim).filter(|o| !o.is_empty()) {
            let o = o.trim_end_matches('/').to_ascii_lowercase();
            if !a.origins.iter().any(|x| x == "*" || *x == o) {
                return Err(Refusal::Origin(o));
            }
        }
    }
    Ok(())
}

/// `Host`'s name (its port and an IPv6 literal's brackets dropped) is an IP, `localhost` or a `*.localhost`, or a
/// name the endpoint was told it is reached by.
fn host_ok(host: &str, names: &[String]) -> bool {
    let h = host.trim().to_ascii_lowercase();
    let name = if let Some(rest) = h.strip_prefix('[') {
        // `[::1]:9222`
        match rest.split_once(']') {
            Some((ip, _)) => return ip.parse::<std::net::Ipv6Addr>().is_ok(),
            None => return false,
        }
    } else {
        match h.rsplit_once(':') {
            Some((n, port)) if port.chars().all(|c| c.is_ascii_digit()) => n.to_string(),
            _ => h.clone(),
        }
    };
    let name = name.trim_end_matches('.');
    name.parse::<std::net::IpAddr>().is_ok() || name == "localhost" || name.ends_with(".localhost") || names.iter().any(|n| n == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(origins: &[&str], hosts: &[&str]) -> Allowed {
        Allowed { origins: origins.iter().map(|s| s.to_string()).collect(), hosts: hosts.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn a_library_with_no_origin_is_let_in() {
        assert_eq!(check(Some("127.0.0.1:9222"), None, true, &a(&[], &[])), Ok(()));
        assert_eq!(check(None, None, true, &a(&[], &[])), Ok(()));
    }

    #[test]
    fn a_page_s_origin_is_refused_unless_allowed() {
        let none = a(&[], &[]);
        assert_eq!(check(Some("127.0.0.1:9222"), Some("https://evil.example"), true, &none), Err(Refusal::Origin("https://evil.example".into())));
        // the endpoint's own origin too, as Chrome refuses it
        assert!(check(Some("127.0.0.1:9222"), Some("http://127.0.0.1:9222"), true, &none).is_err());
        let some = a(&["https://app.example"], &[]);
        assert_eq!(check(Some("127.0.0.1:9222"), Some("https://APP.example/"), true, &some), Ok(()));
        assert!(check(Some("127.0.0.1:9222"), Some("https://other.example"), true, &some).is_err());
        assert_eq!(check(Some("127.0.0.1:9222"), Some("https://any.example"), true, &a(&["*"], &[])), Ok(()));
    }

    #[test]
    fn devtools_is_an_origin_like_any_other() {
        assert!(check(Some("127.0.0.1:9222"), Some("devtools://devtools"), true, &a(&[], &[])).is_err());
        assert_eq!(check(Some("127.0.0.1:9222"), Some("devtools://devtools"), true, &a(&["devtools://devtools"], &[])), Ok(()));
    }

    #[test]
    fn origin_is_checked_on_handshakes_only() {
        assert_eq!(check(Some("127.0.0.1:9222"), Some("https://evil.example"), false, &a(&[], &[])), Ok(()));
    }

    #[test]
    fn a_rebound_name_is_refused() {
        let none = a(&[], &[]);
        for ok in ["127.0.0.1:9222", "127.0.0.1", "localhost:9222", "LOCALHOST", "app.localhost:1", "[::1]:9222", "10.0.0.5:9222", "localhost."] {
            assert_eq!(check(Some(ok), None, false, &none), Ok(()), "{ok}");
        }
        for bad in ["evil.example:9222", "evil.example", "127.0.0.1.evil.example", "[evil]:1", "localhost.evil.example"] {
            assert_eq!(check(Some(bad), None, false, &none), Err(Refusal::Host), "{bad}");
        }
        assert_eq!(check(Some("korvus.example:443"), None, true, &a(&[], &["korvus.example"])), Ok(()));
    }

    #[test]
    fn the_refusals_answer_as_chrome_does() {
        assert!(Refusal::Host.response().starts_with("HTTP/1.1 500 "));
        let r = Refusal::Origin("https://evil.example".into()).response();
        assert!(r.starts_with("HTTP/1.1 403 ") && r.contains("--allow-origin https://evil.example"));
    }
}
