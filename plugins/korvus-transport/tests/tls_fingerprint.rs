//! GS.1 verification against a THIRD-PARTY TLS echo: the wire as somebody else's server sees it.
//!
//! OPT-IN, AND IT ASSERTS. Two things changed here on 2026-09-12 (L7d) and both were necessary:
//!
//! 1. It used to `println!` the echo's fields and assert only `status == 200`, catching an `Err` and
//!    printing "NETWORK/TLS ERROR (offline?)" -- so offline it passed having measured nothing, which
//!    is what a test that is never compiled also looks like. It now asserts the fingerprint.
//! 2. It used to be unreachable in practice: nothing in this crate's tests named a TLS profile, so
//!    `fetch` returned "the transport has no TLS profile" before a socket was opened, and the gate
//!    made no outbound connection. That accident is gone twice over -- `tests/clienthello.rs` fetches
//!    in the same process, and the wire is per-request so no test can leave one
//!    lying around for another -- and a gate does not get to depend on a host on the internet, so
//!    this runs only when asked for by name.
//!
//! An external cross-check can compare
//! against the Chrome installed on the box. `tests/clienthello.rs` measures the same ClientHello
//! hermetically, on loopback, and is what the gate relies on.
//!
//!   KORVUS_TLS_ECHO=1 cargo test -p korvus-transport --test all -- tls_fingerprint:: --nocapture

const ENDPOINT: &str = "https://tls.peet.ws/api/all";

#[tokio::test]
async fn ja3_is_chrome_like() {
    if std::env::var("KORVUS_TLS_ECHO").as_deref() != Ok("1") {
        eprintln!(
            "tls_fingerprint::ja3_is_chrome_like: SKIPPED (set KORVUS_TLS_ECHO=1 to run). It makes a \
             request to {ENDPOINT}; loopback coverage of the same ClientHello is in \
             tests/clienthello.rs and always runs."
        );
        return;
    }
    // The echo is only meaningful when the request names the wire it wants measured. It used to be a
    // process-wide `configure`; the wire rides on the request now, which is also why
    // this test can no longer be affected by whatever else the binary fetched first.
    let req = transport::FetchRequest {
        wire: Some(spec::TlsProfile { id: "chrome_149".into(), platform: "linux".into(), ..Default::default() }),
        ..transport::FetchRequest::navigation(ENDPOINT)
    };

    let r = transport::fetch(req)
        .await
        .expect("KORVUS_TLS_ECHO=1 was set, so the echo must answer; it did not");
    assert_eq!(r.status, 200, "TLS echo should return 200");

    let field = |name: &str| -> String {
        let pat = format!("\"{name}\":\"");
        let i = r.body.find(&pat).unwrap_or_else(|| panic!("no {name} in the echo body"));
        let rest = &r.body[i + pat.len()..];
        rest[..rest.find('"').expect("unterminated field")].to_string()
    };
    let ja4 = field("ja4");
    eprintln!("echo: ja3={} ja4={ja4}", field("ja3_hash"));
    // The shape, not the exact hash: the endpoint sees SNI and a real ALPN negotiation, so the
    // counts differ from the loopback capture by construction. An exact value belongs to
    // clienthello.rs, which controls the whole connection.
    assert!(ja4.starts_with("t13d"), "a Chrome-like client offers TLS 1.3 with SNI; got {ja4}");
    assert!(
        ja4.contains("h2_"),
        "a Chrome-like client's first ALPN entry is h2; got {ja4}"
    );
}
