//! **THE FALLBACK HALF OF HTTPS-Upgrades**, against a host that has no `https:` at all.
//!
//! The upgrade is the easy half and the dangerous one. Chrome tries `https:` for a top-level `http:`
//! navigation *and falls back*; an engine that only did the first half would break every plain-http
//! site there is, which is a far larger regression than the rows the upgrade closes. So this file
//! proves the fallback, not the upgrade — the upgrade's conditions are pinned by
//! `a_top_level_http_navigation_is_tried_over_https_when_the_host_could_hold_a_certificate` in the
//! crate's unit tests, row for row against the Chrome 150 measurement recorded there.
//!
//! **WHY A PROXY.** The rule only fires on the default port and on a host with a registry-known
//! public suffix, which is exactly what a loopback fixture cannot be: `127.0.0.1` is a non-unique
//! address, `web-platform.test` has no known suffix, and port 80 needs root. A forward proxy moves
//! both origins onto loopback while leaving the URL a real-shaped one — the same trick the replay
//! tier uses. The proxy here refuses every `CONNECT` (so the upgraded attempt fails at the transport
//! level, as it would against a host with nothing listening on 443) and answers every plain `GET`.
//!
//! **WHY A CHILD PROCESS.** A proxy is configured through the environment, and the environment is
//! process-global: setting it in this binary would put every other integration test in this crate
//! behind the proxy. So the test re-runs THIS test binary, one test, with the variables set.
//!
//! What the child must produce, and what this asserts:
//!
//! ```text
//! CONNECT <host>:443        the upgrade attempt      -- absent => no upgrade happened at all
//! GET http://<host>/        the fallback             -- absent => the upgrade had no way back
//! ```

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;

/// The host the child navigates to. Not a corpus site and not a real one: what the rule needs is a
/// registry-known suffix and the default port, and the proxy answers for whatever it is told.
const HOST: &str = "korvus-c4-no-https.example.com";

/// Set on the child, and the whole difference between the two runs of this binary.
const CHILD_ENV: &str = "KORVUS_C4_UPGRADE_PROXY";

/// One forward proxy that REFUSES every `CONNECT` and answers every plain `GET`, reporting each
/// request line it saw.
fn start_proxy() -> (u16, mpsc::Receiver<String>) {
    let srv = TcpListener::bind("127.0.0.1:0").expect("bind loopback proxy");
    let port = srv.local_addr().expect("proxy addr").port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for conn in srv.incoming() {
            let Ok(conn) = conn else { continue };
            let tx = tx.clone();
            std::thread::spawn(move || serve(conn, tx));
        }
    });
    (port, rx)
}

fn serve(mut conn: TcpStream, tx: mpsc::Sender<String>) {
    let mut r = BufReader::new(conn.try_clone().expect("clone conn"));
    let mut line = String::new();
    if r.read_line(&mut line).is_err() || line.is_empty() {
        return;
    }
    loop {
        let mut h = String::new();
        match r.read_line(&mut h) {
            Ok(0) => break,
            Ok(_) if h == "\r\n" || h == "\n" => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let request_line = line.trim_end().to_string();
    let _ = tx.send(request_line.clone());
    let out: &[u8] = if request_line.starts_with("CONNECT ") {
        // Nothing is listening on 443 for this host, and this is what that looks like through a
        // proxy: the tunnel is refused, so the upgraded attempt never becomes a request.
        b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n"
    } else {
        b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 46\r\nConnection: close\r\n\r\n\
          <!doctype html><title>t</title>PLAIN-HTTP-BODY"
    };
    let _ = conn.write_all(out);
    let _ = conn.flush();
}

/// THE CHILD. A no-op unless this binary was re-run with the proxy named in the environment, which
/// only `the_upgrade_of_an_http_only_host_falls_back_to_http` below ever does.
#[test]
fn the_child_navigation_that_runs_behind_the_proxy() {
    let Ok(_proxy) = std::env::var(CHILD_ENV) else { return };
    let url = format!("http://{HOST}/");
    let mut req = transport::FetchRequest::navigation(url.clone());
    transport::apply_persona_headers(&persona::CapturedPersona::shipped(), &mut req);
    let out = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt")
            .block_on(async move { transport::fetch(req).await })
    })
    .join()
    .expect("driver thread");
    let resp = out.expect("the navigation must succeed: the fallback is what makes it succeed");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.url, url, "the document's URL is the http: one it fell back to, as Chrome reports it");
    assert!(resp.body.contains("PLAIN-HTTP-BODY"), "body was {:?}", resp.body);
}

#[test]
fn the_upgrade_of_an_http_only_host_falls_back_to_http() {
    let (port, seen) = start_proxy();
    let proxy = format!("http://127.0.0.1:{port}");
    let out = std::process::Command::new(std::env::current_exe().expect("current test binary"))
        .args(["--exact", "https_upgrade::the_child_navigation_that_runs_behind_the_proxy", "--nocapture"])
        .env(CHILD_ENV, &proxy)
        .env("ALL_PROXY", &proxy)
        .env("HTTP_PROXY", &proxy)
        .env("HTTPS_PROXY", &proxy)
        .env("http_proxy", &proxy)
        .env("https_proxy", &proxy)
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .output()
        .expect("re-run this test binary for the child");

    let mut lines = Vec::new();
    while let Ok(l) = seen.recv_timeout(std::time::Duration::from_secs(2)) {
        lines.push(l);
    }
    assert!(
        out.status.success(),
        "the child navigation failed -- an upgrade with no fallback is exactly this failure:\n\
         {}\n{}\nproxy saw: {lines:#?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let connect = format!("CONNECT {HOST}:443");
    assert!(
        lines.iter().any(|l| l.starts_with(&connect)),
        "no `{connect}` -- the navigation was never upgraded, so this test proves nothing.\nsaw: {lines:#?}"
    );
    let get = format!("GET http://{HOST}/ ");
    assert!(
        lines.iter().any(|l| l.starts_with(&get)),
        "the upgrade was made and never fell back: `{get}` was never sent.\nsaw: {lines:#?}"
    );
    // ...and nothing was sent BEFORE the CONNECT: the upgrade is tried first, not alongside.
    let first = lines.first().map(String::as_str).unwrap_or("");
    assert!(first.starts_with(&connect), "the first thing on the wire was {first:?}, not the upgrade");
}
