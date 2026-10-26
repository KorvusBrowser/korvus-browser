//! What a page's requests carry over HTTP/2, and a response's `statusText`, as Chrome 150.
//!
//! * Every HEADERS frame this engine ever sent carried the connection's one fixed priority -- weight byte 255,
//!   exclusive on the root (Chrome 150 sent 219 for a `u=1` favicon). Chromium
//!   derives both from the request's priority (`Http2PriorityDependencies`, the vendored `http2`'s
//!   `ChromiumPriority`).
//! * A synchronous XHR went out `u=1, i` where Chrome's is `u=0, i` (`FetchParameters::MakeSynchronous`).
//! * `statusText` was the status code's canonical phrase on every response; Chromium's is the status line's own
//!   text, and none over h2 (`HttpResponseHeaders::GetStatusText`).
//!
//! `fixtures/h2-priority/`: `page.html` (quiet 400 ms, a synchronous XHR, three fetches at default/low/high priority
//! in one task, four HTTP/1.1 status lines, a report), `record.py` (an h2-only TLS server answering ~150 ms late,
//! recording each HEADERS frame) and `chrome-150.json` -- stock Chrome 150.0.7871.46, headed under Xvfb, no DevTools
//! client, three runs: the statusTexts and every request's field, exclusive bit and weight alike in all three; the two
//! concurrent fetches `/fl` and `/fh` reached the wire in either order (`runs`), each depending on whichever open
//! stream Chromium's rule names. Here the same server is `http2`'s, its client's bytes teed off the TLS stream for the
//! priority fields; it runs on a runtime of its own, because the page's synchronous XHR holds the test's thread.
//! Before this change (`korvus-b3`): statusText `OK` over h2 and `OK`/`OK`/`OK`/`Not Found` over HTTP/1.1, the
//! synchronous XHR `u=1, i`, `fetch(u, {priority: 'low'})` `u=1, i`, and weight byte 255 on the root for every stream.
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/h2-priority");
const TLS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/wire-spelling");
/// How late the server answers every request but `/` and `/report` (`record.py`'s default).
const DELAY: Duration = Duration::from_millis(150);

/// A stream whose incoming bytes are copied as they are read: the decrypted HTTP/2 a client sent.
struct Tee<S> {
    inner: S,
    seen: Arc<Mutex<Vec<u8>>>,
}

impl<S: AsyncRead + Unpin> AsyncRead for Tee<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let r = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &r {
            let fresh = buf.filled()[before..].to_vec();
            self.seen.lock().unwrap().extend_from_slice(&fresh);
        }
        r
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Tee<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// What the server saw, in order: a request's arrival `(connection, stream, :path, priority)`, or its response's last
/// byte sent `(connection, stream)`; and per connection, the client's bytes.
enum Ev {
    Arrived(usize, u32, String, Option<String>),
    Answered(usize, u32),
}

#[derive(Default)]
struct Seen {
    conns: Vec<Arc<Mutex<Vec<u8>>>>,
    log: Vec<Ev>,
}

fn acceptor() -> Arc<btls::ssl::SslAcceptor> {
    use btls::ssl::{select_next_proto, AlpnError, SslAcceptor, SslFiletype, SslMethod};
    let mut b = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls()).expect("an acceptor");
    b.set_certificate_chain_file(format!("{TLS_DIR}/tls-leaf.pem")).expect("the certificate");
    b.set_private_key_file(format!("{TLS_DIR}/tls-leaf.key"), SslFiletype::PEM).expect("the key");
    b.set_alpn_select_callback(|_, client| select_next_proto(b"\x02h2", client).ok_or(AlpnError::NOACK));
    Arc::new(b.build())
}

async fn serve_conn(tcp: tokio::net::TcpStream, acc: Arc<btls::ssl::SslAcceptor>, page: Arc<Vec<u8>>, seen: Arc<Mutex<Seen>>) {
    let Ok(ssl) = btls::ssl::Ssl::new(acc.context()) else { return };
    let Ok(stream) = tokio_btls::SslStream::new(ssl, tcp) else { return };
    let mut stream = Box::pin(stream);
    if stream.as_mut().accept().await.is_err() {
        return;
    }
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let conn = {
        let mut s = seen.lock().unwrap();
        s.conns.push(bytes.clone());
        s.conns.len() - 1
    };
    let Ok(mut h2) = http2::server::handshake(Tee { inner: stream, seen: bytes }).await else { return };
    while let Some(Ok((req, mut respond))) = h2.accept().await {
        let path = req.uri().path().to_string();
        let priority = req.headers().get("priority").and_then(|v| v.to_str().ok()).map(str::to_string);
        let sid = u32::from(respond.stream_id());
        seen.lock().unwrap().log.push(Ev::Arrived(conn, sid, path.clone(), priority));
        let (page, seen) = (page.clone(), seen.clone());
        tokio::spawn(async move {
            let mut body = req.into_body();
            while let Some(Ok(chunk)) = body.data().await {
                let _ = body.flow_control().release_capacity(chunk.len());
            }
            if path != "/" && path != "/report" {
                tokio::time::sleep(DELAY).await;
            }
            let (ctype, out) = if path == "/" { ("text/html", (*page).clone()) } else { ("text/plain", b"ok".to_vec()) };
            let head = http::Response::builder().status(200).header("content-type", ctype).header("cache-control", "no-store").body(()).unwrap();
            if let Ok(mut send) = respond.send_response(head, false) {
                let _ = send.send_data(bytes::Bytes::from(out), true);
            }
            seen.lock().unwrap().log.push(Ev::Answered(conn, sid));
        });
    }
}

/// `record.py`'s HTTP/1.1 origin: status lines with their own reason phrases.
fn serve_h1() -> u16 {
    use std::io::{BufRead, BufReader, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut w = s;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                    loop {
                        let mut h = String::new();
                        if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                            break;
                        }
                    }
                    let status = match path.as_str() {
                        "/r-canon" => "HTTP/1.1 200 OK",
                        "/r-awesome" => "HTTP/1.1 200 Awesome  ",
                        "/r-none" => "HTTP/1.1 200",
                        "/r-404" => "HTTP/1.1 404 Nope",
                        _ => "HTTP/1.1 404 Not Found",
                    };
                    let resp = format!("{status}\r\nAccess-Control-Allow-Origin: *\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\nok");
                    if w.write_all(resp.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

/// Every HEADERS frame's priority fields in a connection's client bytes: stream -> (exclusive, dependency, weight byte).
fn priorities(bytes: &[u8]) -> HashMap<u32, (u8, u32, u8)> {
    let mut out = HashMap::new();
    let mut p = 24; // the client preface
    while p + 9 <= bytes.len() {
        let len = u32::from_be_bytes([0, bytes[p], bytes[p + 1], bytes[p + 2]]) as usize;
        let (ty, flags) = (bytes[p + 3], bytes[p + 4]);
        let sid = u32::from_be_bytes([bytes[p + 5], bytes[p + 6], bytes[p + 7], bytes[p + 8]]) & 0x7fff_ffff;
        let pl = &bytes[(p + 9).min(bytes.len())..(p + 9 + len).min(bytes.len())];
        if ty == 1 && flags & 0x20 != 0 {
            let q = if flags & 0x08 != 0 { 1 } else { 0 };
            if pl.len() >= q + 5 {
                let dep = u32::from_be_bytes([pl[q], pl[q + 1], pl[q + 2], pl[q + 3]]);
                out.insert(sid, ((dep >> 31) as u8, dep & 0x7fff_ffff, pl[q + 4]));
            }
        }
        p += 9 + len;
    }
    out
}

async fn run() {
    let chrome: serde_json::Value = serde_json::from_slice(&std::fs::read(format!("{FIX}/chrome-150.json")).unwrap()).expect("chrome-150.json");
    let h1 = serve_h1();
    let page = Arc::new(std::fs::read_to_string(format!("{FIX}/page.html")).unwrap().replace("{{H1}}", &format!("http://127.0.0.1:{h1}")).into_bytes());
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.set_nonblocking(true).unwrap();
    let port = l.local_addr().unwrap().port();
    {
        let (page, seen) = (page.clone(), seen.clone());
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
            rt.block_on(async move {
                let l = tokio::net::TcpListener::from_std(l).unwrap();
                let acc = acceptor();
                loop {
                    let Ok((tcp, _)) = l.accept().await else { continue };
                    tokio::spawn(serve_conn(tcp, acc.clone(), page.clone(), seen.clone()));
                }
            });
        });
    }
    let url = format!("https://127.0.0.1:{port}/");
    let p = persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = engine::Engine::new(Box::new(p));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    let mut out = serde_json::Value::Null;
    while start.elapsed() < Duration::from_secs(20) {
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "window.__out ? JSON.stringify(window.__out) : ''".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            out = serde_json::from_str(s).expect("the page's JSON");
            break;
        }
    }
    assert!(out.is_object(), "the page never finished");
    // the report, and a moment for its frames
    let _ = e.pump(Duration::from_millis(500)).await;

    // statusText, over h2 and HTTP/1.1
    assert_eq!(out, chrome["report"], "statusText: korvus vs Chrome 150");

    // Every HEADERS frame: its `priority` field, exclusive bit and weight byte as Chrome 150 sent them for the same
    // request; and the stream it depends on as Chromium's rule gives it (`Http2PriorityDependencies`: the newest
    // open stream of its own urgency, else of the nearest higher one, else the root) over what this server saw --
    // the order the requests arrived in, and which had been answered by then. The rule is graded over THIS run's
    // order, not Chrome's (`depends_on` in the recording): two concurrent `fetch()`es may reach the wire in either
    // order, and the parent follows it.
    let s = seen.lock().unwrap();
    let frames: Vec<HashMap<u32, (u8, u32, u8)>> = s.conns.iter().map(|b| priorities(&b.lock().unwrap())).collect();
    let urgency = |p: &Option<String>| -> usize {
        p.as_deref()
            .and_then(|f| f.split(',').map(str::trim).find_map(|x| x.strip_prefix("u=")).and_then(|u| u.parse::<usize>().ok()))
            .unwrap_or(3)
            .min(7)
    };
    let mut lists: HashMap<usize, Vec<Vec<u32>>> = HashMap::new();
    let mut open: std::collections::HashSet<(usize, u32)> = std::collections::HashSet::new();
    let mut got = serde_json::Map::new();
    let mut diffs = Vec::new();
    for ev in &s.log {
        match ev {
            Ev::Arrived(c, sid, path, priority) => {
                let l = lists.entry(*c).or_insert_with(|| vec![Vec::new(); 8]);
                for li in l.iter_mut() {
                    li.retain(|x| open.contains(&(*c, *x)));
                }
                let u = urgency(priority);
                let want = (0..=u).rev().find_map(|p| l[p].last().copied()).unwrap_or(0);
                l[u].push(*sid);
                open.insert((*c, *sid));
                let (excl, dep, weight) = frames[*c].get(sid).copied().unwrap_or((9, 9, 0));
                if dep != want {
                    diffs.push(format!("{path}: depends on stream {dep}, Chromium's rule gives {want}"));
                }
                got.insert(path.clone(), serde_json::json!({ "priority": priority, "exclusive": excl, "weight": weight }));
            }
            Ev::Answered(c, sid) => {
                open.remove(&(*c, *sid));
            }
        }
    }
    for row in chrome["headers"].as_array().expect("headers") {
        let path = row["path"].as_str().unwrap();
        let want = serde_json::json!({ "priority": row["priority"], "exclusive": row["exclusive"], "weight": row["weight"] });
        if got.get(path) != Some(&want) {
            diffs.push(format!("{path}: korvus {} vs Chrome 150 {want}", got.get(path).cloned().unwrap_or_default()));
        }
    }
    assert!(diffs.is_empty(), "HEADERS priorities differ:\n{}", diffs.join("\n"));
}

/// In a CHILD process of this test binary, as `wire_spelling_live`'s TLS case: the transport reads `KORVUS_EXTRA_CA`
/// when it first builds a wire's client, so the test CA must be in the environment before any request of the binary.
#[tokio::test]
async fn a_pages_h2_priorities_and_status_texts_are_chromes() {
    if std::env::var_os("H2_PRIORITY_CHILD").is_some() {
        run().await;
        return;
    }
    let out = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args(["a_pages_h2_priorities_and_status_texts_are_chromes", "--test-threads=1", "--nocapture"])
        .env("H2_PRIORITY_CHILD", "1")
        .env("KORVUS_EXTRA_CA", format!("{TLS_DIR}/tls-ca.pem"))
        .output()
        .expect("the child test process");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success() && text.contains("1 passed"), "in its own process:\n{text}");
}
