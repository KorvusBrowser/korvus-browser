//! A NEW CONNECTION'S PHASES, as stock Chrome 150 reports them: a device-check script's device check on allegro.pl
//! sent korvus to its captcha 7/7 (rule 579924; Chrome passes 7/7) because its frame's navigation entry read a TCP
//! time of 0 on a FRESH TLS connection -- `domainLookupStart`, `domainLookupEnd`, `connectStart` and
//! `secureConnectionStart` were all `fetchStart`, which Chrome answers only for a connection the request REUSED.
//! Proved both ways by editing only a device-check script's payload.
//!
//! The probe (`fixtures/connect-timing/`, recorded by `record.py` from `/usr/bin/google-chrome` 150.0.7871.46,
//! headed, no DevTools, three runs alike): an HTTPS page on 127.0.0.1 makes a cross-site frame on localhost, both
//! reached through a CONNECT proxy that holds every tunnel 40 ms, so a new connection's TCP phase (which behind a
//! proxy spans the tunnel's setup: `connectStart` -> `secureConnectionStart`) has a known length on loopback. The
//! frame reports its navigation entry, `performance.timing`, and the resource entries of two fetches on its own
//! connection (reused), one to another origin with Timing-Allow-Origin (a new connection) and one without. Rows are
//! shapes and bounds, never milliseconds: Chrome's TCP reads 38.2-38.4 ms for the 40 ms hold (its navigation
//! preconnect starts the connect before the request, and the blocking-time conversion moves it to the request's
//! start), korvus's the full hold.
//!
//! **WHY A CHILD PROCESS** (as `korvus-transport`'s `https_upgrade`): the proxy and the extra root CA are configured
//! through the environment, which is process-global -- set in this binary, it would put every other test behind the
//! proxy. So the servers run here and the engine runs in this binary again, one test, with the variables set.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// How long the proxy holds each tunnel before answering `200 Connection established` (the frame's `HOLD`).
const HOLD: Duration = Duration::from_millis(40);

/// Set on the child: the page's URL. Its absence makes the child test a no-op.
const CHILD_ENV: &str = "KORVUS_CONNECT_TIMING_CHILD_URL";

const PAGE: &str = include_str!("fixtures/connect-timing/page.html");
const FRAME: &str = include_str!("fixtures/connect-timing/frame.html");

/// A throwaway root and a leaf for 127.0.0.1 and localhost it signed: `(root PEM, leaf, leaf key)`.
fn certificates() -> (Vec<u8>, btls::x509::X509, btls::pkey::PKey<btls::pkey::Private>) {
    use btls::{
        asn1::Asn1Time,
        bn::BigNum,
        ec::{EcGroup, EcKey},
        hash::MessageDigest,
        nid::Nid,
        pkey::PKey,
        x509::{
            extension::{BasicConstraints, ExtendedKeyUsage, KeyUsage, SubjectAlternativeName},
            X509NameBuilder, X509,
        },
    };
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
    let key = || PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap();
    let name = |cn: &str| {
        let mut n = X509NameBuilder::new().unwrap();
        n.append_entry_by_text("CN", cn).unwrap();
        n.build()
    };
    let (ca_key, leaf_key) = (key(), key());
    let ca_name = name("korvus connect-timing test root");
    let mut ca = X509::builder().unwrap();
    ca.set_version(2).unwrap();
    ca.set_serial_number(&BigNum::from_u32(1).unwrap().to_asn1_integer().unwrap()).unwrap();
    ca.set_subject_name(&ca_name).unwrap();
    ca.set_issuer_name(&ca_name).unwrap();
    ca.set_pubkey(&ca_key).unwrap();
    ca.set_not_before(&Asn1Time::days_from_now(0).unwrap()).unwrap();
    ca.set_not_after(&Asn1Time::days_from_now(2).unwrap()).unwrap();
    ca.append_extension(&BasicConstraints::new().critical().ca().build().unwrap()).unwrap();
    ca.append_extension(&KeyUsage::new().critical().key_cert_sign().build().unwrap()).unwrap();
    ca.sign(&ca_key, MessageDigest::sha256()).unwrap();
    let ca = ca.build();
    let mut leaf = X509::builder().unwrap();
    leaf.set_version(2).unwrap();
    leaf.set_serial_number(&BigNum::from_u32(2).unwrap().to_asn1_integer().unwrap()).unwrap();
    leaf.set_subject_name(&name("127.0.0.1")).unwrap();
    leaf.set_issuer_name(ca.subject_name()).unwrap();
    leaf.set_pubkey(&leaf_key).unwrap();
    leaf.set_not_before(&Asn1Time::days_from_now(0).unwrap()).unwrap();
    leaf.set_not_after(&Asn1Time::days_from_now(2).unwrap()).unwrap();
    leaf.append_extension(&BasicConstraints::new().build().unwrap()).unwrap();
    leaf.append_extension(&KeyUsage::new().critical().digital_signature().build().unwrap()).unwrap();
    leaf.append_extension(&ExtendedKeyUsage::new().server_auth().build().unwrap()).unwrap();
    let san = SubjectAlternativeName::new().ip("127.0.0.1").dns("localhost").build(&leaf.x509v3_context(Some(&ca), None)).unwrap();
    leaf.append_extension(&san).unwrap();
    leaf.sign(&ca_key, MessageDigest::sha256()).unwrap();
    (ca.to_pem().unwrap(), leaf.build(), leaf_key)
}

/// One keep-alive HTTPS connection: the probe's routes, as `record.py` answers them.
fn serve(mut s: btls::ssl::SslStream<TcpStream>) {
    let mut pending: Vec<u8> = Vec::new();
    loop {
        // one request head (and its body, by Content-Length)
        let head_end = loop {
            if let Some(i) = pending.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            let mut buf = [0u8; 4096];
            match s.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => pending.extend_from_slice(&buf[..n]),
            }
        };
        let head = String::from_utf8_lossy(&pending[..head_end]).to_string();
        let len = BufReader::new(head.as_bytes())
            .lines()
            .map_while(Result::ok)
            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
            .unwrap_or(0);
        while pending.len() < head_end + len {
            let mut buf = [0u8; 4096];
            match s.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => pending.extend_from_slice(&buf[..n]),
            }
        }
        pending.drain(..head_end + len);
        let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
        let (ctype, body, extra): (&str, &str, &str) = match path.as_str() {
            "/" => ("text/html; charset=utf-8", PAGE, ""),
            "/frame" => ("text/html; charset=utf-8", FRAME, ""),
            "/r1" | "/r2" => ("text/plain", "ok", ""),
            "/tao" => ("text/plain", "ok", "Access-Control-Allow-Origin: *\r\nTiming-Allow-Origin: *\r\n"),
            "/notao" => ("text/plain", "ok", "Access-Control-Allow-Origin: *\r\n"),
            _ => ("text/plain", "", ""),
        };
        let out = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nCache-Control: no-store\r\n{extra}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        if s.write_all(out.as_bytes()).and_then(|_| s.flush()).is_err() {
            return;
        }
    }
}

/// The HTTPS server on 127.0.0.1 (ALPN `http/1.1`, as the recorder's): its port and the root's PEM.
fn https_server() -> (u16, Vec<u8>) {
    use btls::ssl::{AlpnError, SslAcceptor, SslMethod};
    let (root, leaf, key) = certificates();
    let mut b = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls()).unwrap();
    b.set_certificate(&leaf).unwrap();
    b.set_private_key(&key).unwrap();
    b.set_alpn_select_callback(|_, client| btls::ssl::select_next_proto(b"\x08http/1.1", client).ok_or(AlpnError::NOACK));
    let acceptor = std::sync::Arc::new(b.build());
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let a = acceptor.clone();
            std::thread::spawn(move || {
                if let Ok(tls) = a.accept(s) {
                    serve(tls);
                }
            });
        }
    });
    (port, root)
}

fn pipe(mut from: TcpStream, mut to: TcpStream) {
    let _ = std::io::copy(&mut from, &mut to);
    let _ = to.shutdown(std::net::Shutdown::Both);
    let _ = from.shutdown(std::net::Shutdown::Both);
}

/// A CONNECT proxy that holds each tunnel [`HOLD`] before answering (the recorder's): its port.
fn delay_proxy() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for c in l.incoming().flatten() {
            std::thread::spawn(move || {
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim_end().is_empty() {
                        break;
                    }
                }
                let mut c = c;
                let target = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let (Some("CONNECT"), Some((_, p))) = (line.split_whitespace().next(), target.rsplit_once(':')) else {
                    let _ = c.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n");
                    return;
                };
                let Ok(up) = TcpStream::connect(("127.0.0.1", p.parse::<u16>().unwrap_or(0))) else { return };
                std::thread::sleep(HOLD);
                if c.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").is_err() {
                    return;
                }
                let (c2, up2) = (c.try_clone().unwrap(), up.try_clone().unwrap());
                std::thread::spawn(move || pipe(c2, up2));
                pipe(up, c);
            });
        }
    });
    port
}

/// THE CHILD: the page as `korvus fetch` loads it, behind the proxy the parent named; prints the frame's report.
#[tokio::test]
async fn the_child_page_behind_the_delay_proxy() {
    let Ok(url) = std::env::var(CHILD_ENV) else { return };
    let p = persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = engine::Engine::new(Box::new(p));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "String(window.__r || '')".to_string()).expect("eval");
        if let Some(r) = v.as_str().filter(|r| !r.is_empty()) {
            println!("CONNECT-TIMING-REPORT {r}");
            return;
        }
    }
    panic!("the frame never reported");
}

#[test]
fn a_new_connections_phases_are_reported_as_chrome_150_reports_them() {
    let (port, root) = https_server();
    let proxy = format!("http://127.0.0.1:{}", delay_proxy());
    let dir = std::env::temp_dir().join(format!("korvus-connect-timing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ca = dir.join("root.pem");
    std::fs::write(&ca, &root).unwrap();
    let out = std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args(["--exact", "connect_timing_live::the_child_page_behind_the_delay_proxy", "--nocapture"])
        .env(CHILD_ENV, format!("https://127.0.0.1:{port}/"))
        .env("KORVUS_EXTRA_CA", &ca)
        .env("ALL_PROXY", &proxy)
        .env("HTTP_PROXY", &proxy)
        .env("HTTPS_PROXY", &proxy)
        .env("all_proxy", &proxy)
        .env("http_proxy", &proxy)
        .env("https_proxy", &proxy)
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .output()
        .expect("re-run this test binary for the child");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let report = stdout.lines().find_map(|l| l.strip_prefix("CONNECT-TIMING-REPORT ")).unwrap_or_else(|| {
        panic!("the child reported nothing:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr))
    });
    let report: serde_json::Value = serde_json::from_str(report).expect("the report is JSON");
    let got: BTreeMap<String, serde_json::Value> = serde_json::from_value(report["rows"].clone()).expect("rows");
    let want: BTreeMap<String, serde_json::Value> =
        serde_json::from_str(include_str!("fixtures/connect-timing/chrome-150.json")).expect("the fixture");
    let differ: Vec<String> = want
        .iter()
        .filter(|(k, v)| got.get(*k) != Some(v))
        .map(|(k, v)| format!("{k}: Chrome {v}, korvus {}", got.get(k).map(|g| g.to_string()).unwrap_or("-".into())))
        .collect();
    assert!(differ.is_empty(), "rows unlike Chrome 150's:\n{}\nphases: {}", differ.join("\n"), report["info"]);
}
