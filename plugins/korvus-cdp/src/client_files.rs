//! korvus's own client, served from the endpoint's port at `/client/` (D87, D88 step 4).
//!
//! The client is `client` (Apache-2.0, TypeScript, built with Vite); its `build/` is committed, so it is
//! compiled into the engine here with no Node in the engine's build. `korvus serve` prints the address with the
//! endpoint's own viewer ticket in its fragment, which the page reads and no server ever sees. The page connects back
//! to the same port over the viewer channel (`viewer.rs`), so it needs no `--allow-origin`.

const INDEX: &str = include_str!("../../../client/build/index.html");
const APP: &str = include_str!("../../../client/build/app.js");
const BUNDLE: &str = include_str!("../../../client/build/korvus-client.js");

/// The page's policy: its own scripts only, no other origin's anything, the socket back to this port, frames drawn
/// from blobs, and the inline styles a shadow root carries.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; img-src blob: data:; connect-src 'self'; base-uri 'none'; form-action 'none'";

/// The HTTP response for a `/client` request; `None` for any other path.
pub fn response(path: &str) -> Option<String> {
    let path = path.split(['?', '#']).next().unwrap_or("");
    let (ctype, body, csp) = match path {
        // `./app.js` resolves against the directory
        "/client" => return Some("HTTP/1.1 308 Permanent Redirect\r\nLocation: /client/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()),
        "/client/" | "/client/index.html" => ("text/html; charset=utf-8", INDEX, true),
        "/client/app.js" => ("text/javascript; charset=utf-8", APP, false),
        "/client/korvus-client.js" => ("text/javascript; charset=utf-8", BUNDLE, false),
        p if p.starts_with("/client/") => return Some("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()),
        _ => return None,
    };
    let policy = if csp { format!("Content-Security-Policy: {CSP}\r\n") } else { String::new() };
    Some(format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n{policy}X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_is_served_under_its_policy() {
        let page = response("/client/").unwrap();
        assert!(page.starts_with("HTTP/1.1 200 ") && page.contains("Content-Security-Policy: default-src 'none'") && page.contains("<script type=\"module\" src=\"./app.js\">"));
        assert!(response("/client/korvus-client.js").unwrap().contains("text/javascript"));
        assert!(response("/client").unwrap().starts_with("HTTP/1.1 308 "));
        assert!(response("/client/../etc/passwd").unwrap().starts_with("HTTP/1.1 404 "));
        assert!(response("/json/version").is_none());
    }
}
