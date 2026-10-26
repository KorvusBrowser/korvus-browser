//! **WHEN A REQUEST CARRIES `Origin`**, read off a loopback socket.
//!
//! Fetch's "append a request Origin header" has two rules -- response tainting is `cors`, or the
//! method is neither GET nor HEAD -- and Chromium has a third that the spec does not: a request whose
//! loader configured CORS ITSELF (`FetchParameters::SetCrossOriginAccessControl`: a module script, a
//! `crossorigin`-attributed subresource) carries `Origin` even when it is a same-origin GET.
//!
//! The table below was measured on 2026-09-18, one page, four fresh profiles, headers read off a
//! cleartext loopback socket. Brave 151.1.93.138, Google Chrome 150.0.7871.46, Microsoft Edge
//! 151.0.4129.93 and Opera 135.0.5973.133 agree on every row.
//!
//! ```text
//! request                                    Origin        korvus BEFORE
//! fetch() same-origin GET   (mode cors)      -             -              ok
//! XHR     same-origin GET                    -             -              ok
//! fetch() same-origin POST                   the origin    the origin     ok
//! fetch() cross-origin GET  (mode cors)      the origin    the origin     ok
//! fetch() cross-origin GET  (mode no-cors)   -             -              ok
//! fetch() cross-origin POST (mode no-cors)   the origin    the origin     ok
//! import("<same-origin>.js")                 the origin    NONE           <- G46
//! import("<cross-origin>.js")                the origin    NONE           <- G46
//! new Worker("<same-origin>.js")             -             -              ok
//! a cors request redirected cross-origin     null          null           ok
//! ```
//!
//! The two G46 rows were both empty because a module request carried no `page` at all: it went out
//! with `Sec-Fetch-Mode: cors` and was never tainted, never CORS-checked and never given an origin.
//! `core/korvus-runtime/src/modules.rs` now hands one over, and `cors_preconfigured` is what
//! separates it from a same-origin `fetch()`, which correctly sends none.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;

/// One loopback origin that answers every request and reports the headers it saw.
struct Sink {
    port: u16,
    seen: mpsc::Receiver<Vec<(String, String)>>,
}

impl Sink {
    fn start() -> Sink {
        let srv = TcpListener::bind("127.0.0.1:0").expect("bind loopback sink");
        let port = srv.local_addr().expect("sink addr").port();
        let (tx, seen) = mpsc::channel();
        std::thread::spawn(move || {
            for conn in srv.incoming() {
                let Ok(conn) = conn else { break };
                let tx = tx.clone();
                std::thread::spawn(move || serve(conn, tx));
            }
        });
        Sink { port, seen }
    }

    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// The `Origin` of the next request this sink received, or `Err` describing what went wrong.
    fn next_origin(&self) -> Result<Option<String>, String> {
        let h = self
            .seen
            .recv_timeout(std::time::Duration::from_secs(10))
            .map_err(|_| "the sink received no request within 10s".to_string())?;
        let mut hit = h.iter().filter(|(k, _)| k.eq_ignore_ascii_case("origin"));
        let first = hit.next().map(|(_, v)| v.clone());
        if hit.next().is_some() {
            return Err(format!("Origin was sent more than once: {h:?}"));
        }
        Ok(first)
    }
}

fn serve(mut conn: TcpStream, tx: mpsc::Sender<Vec<(String, String)>>) {
    let mut rd = BufReader::new(conn.try_clone().expect("clone"));
    loop {
        let mut start = String::new();
        if rd.read_line(&mut start).unwrap_or(0) == 0 {
            return;
        }
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            if rd.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let line = line.trim_end().to_string();
            if line.is_empty() {
                break;
            }
            let (k, v) = line.split_once(':').unwrap_or((line.as_str(), ""));
            headers.push((k.to_string(), v.trim().to_string()));
        }
        // Drain a body so a POST's bytes do not become the next request line.
        let len: usize = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(0);
        if len > 0 {
            let mut body = vec![0u8; len];
            use std::io::Read;
            let _ = rd.read_exact(&mut body);
        }
        let _ = tx.send(headers);
        // Permissive CORS, so the cross-origin rows are answered rather than rejected before the
        // header this file is about has been recorded.
        let _ = conn.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\
              Access-Control-Allow-Origin: *\r\nConnection: keep-alive\r\n\r\nok",
        );
        let _ = conn.flush();
    }
}

/// Send one request through the real `transport::fetch`, worn by the shipped persona.
fn send(mut req: transport::FetchRequest) {
    transport::apply_persona_headers(&persona::CapturedPersona::shipped(), &mut req);
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt")
            .block_on(async move { transport::fetch(req).await.err() })
    })
    .join()
    .expect("driver thread");
}

/// A page subresource of `destination`/`mode` from `origin`, as the engine's own loaders build one.
fn page_request(
    url: &str,
    method: &str,
    destination: &str,
    mode: transport::Mode,
    origin: &str,
    cors_preconfigured: bool,
) -> transport::FetchRequest {
    let mut req = transport::FetchRequest::subresource(url, destination, mode, format!("{origin}/"));
    req.method = method.to_string();
    if method != "GET" {
        req.body = Some(Vec::new());
    }
    req.page = Some(transport::PageRequest {
        origin: origin.to_string(),
        author_headers: Vec::new(),
        cors_preconfigured,
    });
    req
}

/// THE WHOLE TABLE, in one connection-sharing run, against what the four browsers were measured
/// sending.
#[test]
fn the_origin_header_is_sent_on_exactly_the_requests_chromium_sends_it_on() {
    use transport::Mode;
    let a = Sink::start();
    let b = Sink::start();
    let origin = a.origin();

    // (label, request, which sink answers, expected Origin)
    let want_none: Option<String> = None;
    let want_origin = Some(origin.clone());

    let rows: Vec<(&str, transport::FetchRequest, &Sink, Option<String>)> = vec![
        (
            "fetch() same-origin GET, mode cors",
            page_request(&a.url("/so-get"), "GET", "", Mode::Cors, &origin, false),
            &a,
            want_none.clone(),
        ),
        (
            "fetch() same-origin POST",
            page_request(&a.url("/so-post"), "POST", "", Mode::Cors, &origin, false),
            &a,
            want_origin.clone(),
        ),
        (
            "fetch() cross-origin GET, mode cors",
            page_request(&b.url("/x-get"), "GET", "", Mode::Cors, &origin, false),
            &b,
            want_origin.clone(),
        ),
        (
            "fetch() cross-origin GET, mode no-cors",
            page_request(&b.url("/x-nocors"), "GET", "", Mode::NoCors, &origin, false),
            &b,
            want_none.clone(),
        ),
        (
            "fetch() cross-origin POST, mode no-cors",
            page_request(&b.url("/x-post-nocors"), "POST", "", Mode::NoCors, &origin, false),
            &b,
            want_origin.clone(),
        ),
        (
            "new Worker(<same-origin>), mode same-origin",
            page_request(&a.url("/worker.js"), "GET", "script", Mode::SameOrigin, &origin, false),
            &a,
            want_none.clone(),
        ),
        // G46: the two rows that were empty.
        (
            "import(<same-origin>.js)",
            page_request(&a.url("/mod-same.js"), "GET", "script", Mode::Cors, &origin, true),
            &a,
            want_origin.clone(),
        ),
        (
            "import(<cross-origin>.js)",
            page_request(&b.url("/mod-cross.js"), "GET", "script", Mode::Cors, &origin, true),
            &b,
            want_origin.clone(),
        ),
    ];

    let mut wrong = Vec::new();
    for (label, req, sink, want) in rows {
        send(req);
        match sink.next_origin() {
            Ok(got) if got == want => {}
            Ok(got) => wrong.push(format!("{label}: sent Origin {got:?}, Chromium sends {want:?}")),
            Err(e) => wrong.push(format!("{label}: {e}")),
        }
    }
    assert!(wrong.is_empty(), "Origin does not match what the four browsers send:\n{}", wrong.join("\n"));
}

/// A request no page made carries no `Origin` at all -- the engine's own navigation, which is what
/// `Sec-Fetch-Site: none` says it is.
#[test]
fn a_request_with_no_page_carries_no_origin() {
    let a = Sink::start();
    send(transport::FetchRequest::navigation(a.url("/")));
    assert_eq!(a.next_origin(), Ok(None), "an engine navigation must not claim an origin");
}

/// RED-PROOF for G46, one flag apart: the SAME same-origin GET in cors mode, with and without the
/// loader's own CORS configuration. Without it Chromium sends nothing; with it Chromium sends the
/// origin, and before FP2 this engine sent nothing either way.
#[test]
fn only_a_loader_configured_cors_request_states_its_origin_same_origin() {
    use transport::Mode;
    let a = Sink::start();
    let origin = a.origin();

    send(page_request(&a.url("/plain"), "GET", "", Mode::Cors, &origin, false));
    assert_eq!(a.next_origin(), Ok(None), "a same-origin fetch() GET must send no Origin");

    send(page_request(&a.url("/module"), "GET", "script", Mode::Cors, &origin, true));
    assert_eq!(
        a.next_origin(),
        Ok(Some(origin.clone())),
        "a module script must state its origin even same-origin -- measured on all four browsers"
    );
}

/// A CORS request redirected to a third origin sends `Origin: null` from then on (Fetch's tainted
/// origin flag). Unchanged by G46, and asserted here so the new branch cannot quietly override it.
#[test]
fn a_cors_redirect_across_origins_taints_the_origin_to_null() {
    use transport::Mode;
    let a = Sink::start();
    let b = Sink::start();
    let c = Sink::start();
    let origin = a.origin();

    // b redirects to c: the first hop carries a's origin, the second carries `null`.
    let redirector = TcpListener::bind("127.0.0.1:0").expect("bind");
    let rport = redirector.local_addr().expect("addr").port();
    let to = c.url("/end");
    std::thread::spawn(move || {
        for conn in redirector.incoming() {
            let Ok(mut conn) = conn else { break };
            let mut rd = BufReader::new(conn.try_clone().expect("clone"));
            loop {
                let mut line = String::new();
                if rd.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                if line.trim_end().is_empty() {
                    break;
                }
            }
            let _ = conn.write_all(
                format!(
                    "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\n\
                     Access-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            );
        }
    });

    let _ = b; // b is only here to keep the ports distinct from a's and c's.
    let req = page_request(
        &format!("http://127.0.0.1:{rport}/redirect"),
        "GET",
        "",
        Mode::Cors,
        &origin,
        false,
    );
    send(req);
    assert_eq!(
        c.next_origin(),
        Ok(Some("null".to_string())),
        "after a cross-origin redirect a CORS request's Origin is `null`"
    );
}
