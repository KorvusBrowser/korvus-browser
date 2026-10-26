//! WHICH subresources load: the favicon and the manifest's icon, `loading=lazy`,
//! scripts and images inserted into an `about:blank`/`srcdoc` frame, and `srcset`/`<picture>` at the
//! persona's device pixel ratio.
//!
//! Every expected set below is CHROME'S ACCESS LOG for the same markup: `/usr/bin/google-chrome`
//! 150.0.7871.46 driven over CDP by `tools/parity/driver.py`'s own `attach()`, one fresh profile and
//! one loopback server per fixture, read after `load` plus a 3 s settle. Headless and headed (Xvfb,
//! the oracle's configuration) gave identical sets on every fixture. The DPR-2 rows come from a probe
//! of our own Chrome started with `--force-device-scale-factor=2` -- a probe, never an oracle flag.
//!
//! THE ASSERTION IS THE SERVER'S LOG, both directions: a path Chrome requested and this engine did
//! not fails, and so does a path this engine requested and Chrome did not. `load_page` is handed the
//! document's bytes, so `/page.html` itself never crosses this wire.
//!
//! PERSONA. Every fixture runs under the captured Chrome 150 bundle. It carries `devicePixelRatio: 1` and `connection.effectiveType: "4g"`, so the DPR-dependent fixtures
//! also run under the Chrome bundle with ONLY its screen's `device_pixel_ratio` replaced by 2 -- which
//! is how a DPR-2 capture would differ -- against the DPR-2 Chrome measurement.
//!
//! The lazy-load grids do not hard-code a cut-off: the measured RULE is asserted (an image is
//! requested iff its top is within `innerHeight + 1250` of the page's top, `innerWidth + 625` for its
//! left; a frame's `+ 2500`), with the viewport read from the engine under test, so a persona with a
//! different window gets the answer Chrome gives for that window.
use engine::Engine;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const BUNDLES: [&str; 1] = ["chrome-152-linux-20261016T1022"];

type Log = Arc<Mutex<Vec<String>>>;

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// A valid `n`x`n` RGBA PNG (stored, uncompressed deflate blocks).
fn png(n: u32) -> Vec<u8> {
    let mut raw = Vec::new();
    for _ in 0..n {
        raw.push(0u8);
        for _ in 0..n {
            raw.extend_from_slice(&[255, 0, 0, 255]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let chunks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, ch) in chunks.iter().enumerate() {
        z.push(u8::from(i + 1 == chunks.len()));
        let len = ch.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(ch);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let chunk = |t: &[u8], d: &[u8]| {
        let mut out = (d.len() as u32).to_be_bytes().to_vec();
        let mut td = t.to_vec();
        td.extend_from_slice(d);
        out.extend_from_slice(&td);
        out.extend_from_slice(&crc32(&td).to_be_bytes());
        out
    };
    let mut ihdr = n.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&n.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut out = vec![0x89, b'P', b'N', b'G', 13, 10, 26, 10];
    out.extend(chunk(b"IHDR", &ihdr));
    out.extend(chunk(b"IDAT", &z));
    out.extend(chunk(b"IEND", &[]));
    out
}

/// An ICO carrying 16 and 32 px PNG frames.
fn ico() -> Vec<u8> {
    let frames = [png(16), png(32)];
    let mut out = vec![0, 0, 1, 0, 2, 0];
    let mut off = 6 + 16 * 2;
    for (s, f) in [16u8, 32].iter().zip(&frames) {
        out.extend_from_slice(&[*s, *s, 0, 0, 1, 0, 32, 0]);
        out.extend_from_slice(&(f.len() as u32).to_le_bytes());
        out.extend_from_slice(&(off as u32).to_le_bytes());
        off += f.len();
    }
    for f in &frames {
        out.extend_from_slice(f);
    }
    out
}

fn manifest(name: &str) -> Option<&'static str> {
    Some(match name {
        "mf.webmanifest" => r#"{"name":"t","short_name":"t","start_url":".","display":"minimal-ui","icons":[{"src":"r192-m.png","sizes":"192x192","type":"image/png"},{"src":"r512-m.png","sizes":"512x512","type":"image/png"}]}"#,
        "mfb.webmanifest" => r#"{"name":"t","display":"browser","icons":[{"src":"r192-mb.png","sizes":"192x192","type":"image/png"}]}"#,
        "mfn.webmanifest" => r#"{"name":"t","display":"standalone"}"#,
        "mfs.webmanifest" => r#"{"name":"t","display":"standalone","icons":[{"src":"r192-ms.png","sizes":"192x192","type":"image/png"}]}"#,
        "mfk.webmanifest" => r#"{"name":"t","start_url":".","display":"standalone","icons":[{"src":"r192-mk.png","sizes":"192x192","purpose":"maskable"}]}"#,
        "mf144.webmanifest" => r#"{"name":"t","start_url":".","display":"standalone","icons":[{"src":"r144-mq.png","sizes":"144x144"},{"src":"r512-mq.png","sizes":"512x512"}]}"#,
        "mfo.webmanifest" => r#"{"name":"t","start_url":".","display":"browser","display_override":["standalone"],"icons":[{"src":"r192-mo.png","sizes":"192x192"}]}"#,
        _ => return None,
    })
}

fn body_for(path: &str) -> (u16, &'static str, Vec<u8>) {
    let leaf = path.rsplit('/').next().unwrap_or("");
    if leaf.starts_with("missing") {
        return (404, "text/plain", b"no".to_vec());
    }
    if let Some(m) = manifest(leaf) {
        return (200, "application/manifest+json", m.as_bytes().to_vec());
    }
    if leaf.ends_with(".html") {
        return (200, "text/html", b"<!doctype html><p>child".to_vec());
    }
    if leaf.ends_with(".mjs") {
        // An ES module that counts its own evaluations, so a test can tell "fetched once" from
        // "evaluated once" -- they are different claims and only the first one is on the wire.
        return (200, "text/javascript", b"globalThis.__m = (globalThis.__m || 0) + 1; export const v = 1;".to_vec());
    }
    if let Some(stem) = leaf.strip_suffix(".js").filter(|s| s.starts_with("ord-") || s.starts_with("slow-ord-")) {
        // Announces its RUN ORDER: the n-th script to run requests `ran-<n>-<stem>.png`.
        return (200, "text/javascript", format!("window.__n = (window.__n || 0) + 1; new Image().src = 'ran-' + window.__n + '-{stem}.png'").into_bytes());
    }
    if let Some(stem) = leaf.strip_suffix(".js") {
        // The script proves it RAN, in the frame, by creating an image from its own `document`.
        return (200, "text/javascript", format!("document.createElement('img').src = '{stem}-exec.png'").into_bytes());
    }
    if leaf.ends_with(".css") {
        return (200, "text/css", b"p{color:red}".to_vec());
    }
    if leaf.ends_with(".svg") {
        return (200, "image/svg+xml", br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32"/></svg>"#.to_vec());
    }
    if leaf.ends_with(".ico") {
        return (200, "image/x-icon", ico());
    }
    if leaf.ends_with(".png") || leaf.ends_with(".jpg") {
        let n = leaf.strip_prefix('r').and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next()).and_then(|d| d.parse().ok()).unwrap_or(1);
        return (200, "image/png", png(n));
    }
    (200, "text/plain", b"x".to_vec())
}

async fn serve(listener: TcpListener, log: Log) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else { return };
        let log = log.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            let Ok(n) = s.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/").split('?').next().unwrap_or("/").to_string();
            if let Ok(mut l) = log.lock() {
                l.push(path.clone());
            }
            if path.contains("/slow-") {
                tokio::time::sleep(Duration::from_millis(600)).await;
            }
            let (status, ctype, body) = body_for(&path);
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(&body).await;
            let _ = s.flush().await;
        });
    }
}

#[derive(Clone, Copy)]
enum Who {
    Bundle(&'static str),
    Dpr2,
}

fn persona(who: Who) -> Box<dyn spec::Persona> {
    match who {
        Who::Bundle(id) => Box::new(persona::CapturedPersona::select(Some(id)).unwrap_or_else(|e| panic!("select {id}: {e}"))),
        Who::Dpr2 => {
            let base = persona::CapturedPersona::select(Some(BUNDLES[0])).expect("chrome bundle");
            let mut screen = spec::Persona::screen(&base);
            screen.device_pixel_ratio = 2.0;
            persona::Overridden::on(base).screen(screen).boxed()
        }
    }
}

struct Run {
    requests: Vec<String>,
    inner: (f64, f64),
}

/// Load `head` + `body`, pump to `load`, settle, optionally run `then` and settle again.
async fn run(who: Who, head: &str, body: &str, then: Option<&str>) -> Run {
    run_delivered(who, &[], head, body, then).await
}

/// The same, with the document DELIVERED under `headers` -- the policy rows need the
/// response's `Content-Security-Policy`, and `/page.html` never crosses this wire, so a header can
/// only reach the engine this way.
async fn run_delivered(who: Who, headers: &[(&str, &str)], head: &str, body: &str, then: Option<&str>) -> Run {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, log.clone()));
    let doc = format!("<!doctype html><html><head><meta charset=utf-8><title>t</title>{head}</head><body>{body}</body></html>");
    let mut e = Engine::new(persona(who));
    e.run().await.expect("boot");
    let delivered: Vec<(String, String)> = std::iter::once(("content-type".to_string(), "text/html; charset=utf-8".to_string()))
        .chain(headers.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())))
        .collect();
    e.load_page_delivered(&doc, &format!("http://{addr}/page.html"), &[], &delivered).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    settle(&mut e, 1500).await;
    if let Some(js) = then {
        e.eval_value("then", js.to_string()).expect("then");
        settle(&mut e, 1500).await;
    }
    let inner = e.eval_value("inner", "[innerWidth, innerHeight]".to_string()).expect("inner");
    let inner = (inner[0].as_f64().unwrap_or(0.0), inner[1].as_f64().unwrap_or(0.0));
    let requests = log.lock().unwrap().clone();
    Run { requests, inner }
}

async fn settle(e: &mut Engine, ms: u64) {
    let until = tokio::time::Instant::now() + Duration::from_millis(ms);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(40), e.run()).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn counts(v: &[String]) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for p in v {
        *m.entry(p.clone()).or_default() += 1;
    }
    m
}

/// The engine's log equals Chrome's, each path exactly once.
fn assert_log(label: &str, who: &str, got: &[String], want: &[&str]) {
    let got = counts(got);
    let want: BTreeMap<String, usize> = want.iter().map(|p| (p.to_string(), 1)).collect();
    assert_eq!(got, want, "{label} under {who}: the access log differs from Chrome's");
}

fn who_name(w: Who) -> &'static str {
    match w {
        Who::Bundle(id) => id,
        Who::Dpr2 => "chrome-150 at devicePixelRatio 2",
    }
}

async fn check(who: Who, label: &str, head: &str, body: &str, then: Option<&str>, want: &[&str]) {
    let r = run(who, head, body, then).await;
    assert_log(label, who_name(who), &r.requests, want);
}

// ---- icons -----------------------------------------------------------------------------------

#[tokio::test]
async fn icons_are_requested_as_chrome_requests_them() {
    let rows: &[(&str, &str, &str, Option<&str>, &[&str])] = &[
        ("no icon link", "", "<p>x</p>", None, &["/favicon.ico"]),
        ("apple-touch-icons only", "<link rel=apple-touch-icon href=a.png><link rel=apple-touch-icon-precomposed href=ap.png>", "", None, &["/favicon.ico"]),
        ("mask-icon only", "<link rel=mask-icon href=m.svg color=red>", "", None, &["/favicon.ico"]),
        ("mask-icon + icon", "<link rel=mask-icon href=m.svg color=red><link rel=icon href=i.png>", "", None, &["/i.png"]),
        ("shortcut icon", "<link rel=\"shortcut icon\" href=rico.ico>", "", None, &["/rico.ico"]),
        ("icon link in body", "", "<link rel=icon href=bodyicon.png>", None, &["/favicon.ico"]),
        ("icon whose media does not match", "<link rel=icon media=\"(max-width: 100px)\" href=r32-narrow.png>", "", None, &["/favicon.ico"]),
        ("16 + 32 + apple 180", "<link rel=icon type=image/png sizes=32x32 href=r32.png><link rel=icon type=image/png sizes=16x16 href=r16.png><link rel=apple-touch-icon sizes=180x180 href=r180-apple.png>", "", None, &["/r32.png"]),
        ("16,32,48,96,192", "<link rel=icon sizes=16x16 href=r16.png><link rel=icon sizes=32x32 href=r32.png><link rel=icon sizes=48x48 href=r48.png><link rel=icon sizes=96x96 href=r96.png><link rel=icon sizes=192x192 href=r192.png>", "", None, &["/r32.png"]),
        ("16,32,192 + any svg", "<link rel=icon sizes=16x16 href=r16.png><link rel=icon sizes=32x32 href=r32.png><link rel=icon sizes=192x192 href=r192.png><link rel=icon sizes=any type=image/svg+xml href=rsvg.svg>", "", None, &["/rsvg.svg"]),
        ("48 + 16", "<link rel=icon sizes=48x48 href=r48.png><link rel=icon sizes=16x16 href=r16.png>", "", None, &["/r16.png"]),
        ("192 + 512", "<link rel=icon sizes=192x192 href=r192.png><link rel=icon sizes=512x512 href=r512.png>", "", None, &["/r192.png"]),
        ("two unsized 32s", "<link rel=icon href=r32-a.png><link rel=icon href=r32-b.png>", "", None, &["/r32-a.png", "/r32-b.png"]),
        ("unsized 16 then sized 32", "<link rel=icon href=r16-nosize.png><link rel=icon sizes=32x32 href=r32.png>", "", None, &["/r16-nosize.png", "/r32.png"]),
        ("sized 32 then unsized 32", "<link rel=icon sizes=32x32 href=r32-s.png><link rel=icon href=r32-n.png>", "", None, &["/r32-n.png"]),
        ("alternate icon + svg icon", "<link rel=\"alternate icon\" href=alt32.png><link rel=icon type=image/svg+xml href=ic.svg>", "", None, &["/ic.svg", "/alt32.png"]),
        ("ico any + svg", "<link rel=icon href=rico.ico sizes=any><link rel=icon type=image/svg+xml href=rsvg.svg>", "", None, &["/rsvg.svg", "/rico.ico"]),
        ("an unknown type", "<link rel=icon type=image/x-foo href=unk.foo>", "", None, &["/unk.foo"]),
        ("a 404 icon", "<link rel=icon href=missing-icon.png>", "", None, &["/missing-icon.png"]),
        (
            "inserted after load, then its href changed",
            "",
            "",
            Some(r#"var l = document.createElement("link"); l.rel = "icon"; l.href = "dyn.png"; document.head.appendChild(l); setTimeout(function () { l.href = "dyn2.png"; }, 300);"#),
            &["/favicon.ico", "/dyn.png", "/dyn2.png"],
        ),
        ("manifest, installable, minimal-ui", "<link rel=manifest href=mf.webmanifest><link rel=icon sizes=48x48 href=r48.png>", "", None, &["/mf.webmanifest", "/r48.png", "/r192-m.png"]),
        ("manifest, display browser", "<link rel=manifest href=mfb.webmanifest><link rel=icon sizes=48x48 href=r48.png>", "", None, &["/mfb.webmanifest", "/r48.png"]),
        ("manifest without icons", "<link rel=manifest href=mfn.webmanifest>", "", None, &["/mfn.webmanifest", "/favicon.ico"]),
        ("manifest without start_url", "<link rel=manifest href=mfs.webmanifest>", "", None, &["/mfs.webmanifest", "/favicon.ico"]),
        ("manifest, maskable icon only", "<link rel=manifest href=mfk.webmanifest>", "", None, &["/mfk.webmanifest", "/favicon.ico"]),
        ("manifest, 144 + 512", "<link rel=manifest href=mf144.webmanifest>", "", None, &["/mf144.webmanifest", "/favicon.ico", "/r144-mq.png"]),
        ("manifest, display_override", "<link rel=manifest href=mfo.webmanifest>", "", None, &["/mfo.webmanifest", "/favicon.ico", "/r192-mo.png"]),
    ];
    for who in BUNDLES {
        for (label, head, body, then, want) in rows {
            check(Who::Bundle(who), label, head, body, *then, want).await;
        }
    }
}

// ---- the icons a Content-Security-Policy stops -----------------------------------

/// AN ICON DOWNLOAD IS A FETCH, and `img-src` governs it.
///
/// Measured over the 504-row replay corpus, this engine asked for an icon on thirteen rows where the
/// oracle asked for none; two of them -- `man.openbsd.org` and `matter-js.github.io/matter-js/demo/`
/// -- were regressions at L0.94, and both documents are delivered with `default-src 'none'`. The
/// brief's other hypothesis, that Chrome skips the default icon for an ERROR document (matter-js's
/// page is a 404), is disproved: a 404 document with no CSP is asked for `/favicon.ico` like any
/// other, and `core/html/csp.rs` carries that row.
///
/// Every `want` below is CHROME'S ACCESS LOG for the same document and the same header, measured the
/// way this file's header describes -- `/usr/bin/google-chrome` 150.0.7871.46 headless, one fresh
/// profile and one loopback server per row, read after `load` plus a settle. An empty `want` is the
/// assertion that matters: Chrome put NOTHING on the wire.
#[tokio::test]
async fn an_icon_a_content_security_policy_forbids_is_never_requested() {
    const DENY: &str = "default-src 'none'; style-src 'self' 'unsafe-inline'";
    const META_DENY: &str = r#"<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'self' 'unsafe-inline'">"#;
    // (label, response headers, head, body, want)
    let rows: &[(&str, &[(&str, &str)], &str, &str, &[&str])] = &[
        ("no policy at all", &[], "", "", &["/favicon.ico"]),
        ("default-src 'none'", &[("content-security-policy", DENY)], "", "", &[]),
        ("default-src 'none', with a declared icon", &[("content-security-policy", DENY)], "<link rel=icon sizes=32x32 href=r32.png>", "", &[]),
        ("img-src 'none'", &[("content-security-policy", "img-src 'none'")], "", "", &[]),
        ("img-src 'none', with a declared icon", &[("content-security-policy", "img-src 'none'")], "<link rel=icon sizes=32x32 href=r32.png>", "", &[]),
        ("an empty img-src source list", &[("content-security-policy", "default-src 'self'; img-src")], "", "", &[]),
        ("IMG-SRC 'NONE' -- names and keywords fold case", &[("content-security-policy", "IMG-SRC 'NONE'")], "", "", &[]),
        ("img-src 'none' 'self' -- 'none' blocks only alone", &[("content-security-policy", "img-src 'none' 'self'")], "", "", &["/favicon.ico"]),
        ("img-src 'self'", &[("content-security-policy", "img-src 'self'")], "", "", &["/favicon.ico"]),
        ("default-src 'self'", &[("content-security-policy", "default-src 'self'")], "", "", &["/favicon.ico"]),
        ("img-src *", &[("content-security-policy", "default-src 'none'; img-src *")], "", "", &["/favicon.ico"]),
        ("script-src 'none' -- another directive is not img-src", &[("content-security-policy", "script-src 'none'")], "", "", &["/favicon.ico"]),
        ("img-src http: on an http document", &[("content-security-policy", "default-src 'none'; img-src http:")], "", "", &["/favicon.ico"]),
        ("img-src https: on an http document", &[("content-security-policy", "default-src 'none'; img-src https:")], "", "", &[]),
        ("img-src data:, and the icon is not a data URL", &[("content-security-policy", "default-src 'none'; img-src data:")], "", "", &[]),
        ("img-src 127.0.0.1:* -- a host-source on any port", &[("content-security-policy", "default-src 'none'; img-src 127.0.0.1:*")], "", "", &["/favicon.ico"]),
        ("img-src 127.0.0.1 -- no port means the DEFAULT port", &[("content-security-policy", "default-src 'none'; img-src 127.0.0.1")], "", "", &[]),
        ("a comma starts a second policy", &[("content-security-policy", "img-src *, img-src 'none'")], "", "", &[]),
        ("a repeated directive: the first wins", &[("content-security-policy", "img-src 'none'; img-src *")], "", "", &[]),
        ("report-only blocks nothing", &[("content-security-policy-report-only", "default-src 'none'")], "", "", &["/favicon.ico"]),
        ("a <meta> in <head> counts", &[], META_DENY, "", &[]),
        ("a <meta> in <body> does NOT", &[], "", META_DENY, &["/favicon.ico"]),
        (
            "img-src 'none' with a manifest: the manifest, and neither icon",
            &[("content-security-policy", "default-src 'self'; img-src 'none'")],
            "<link rel=manifest href=mf.webmanifest>",
            "",
            &["/mf.webmanifest"],
        ),
    ];
    for who in BUNDLES {
        for (label, headers, head, body, want) in rows {
            let r = run_delivered(Who::Bundle(who), headers, head, body, None).await;
            assert_log(label, who_name(Who::Bundle(who)), &r.requests, want);
        }
    }
}

/// A DOCUMENT THAT IS LEFT NEVER HANDS ITS CANDIDATE LIST OVER (L22's E22).
///
/// Blink hands the list to the favicon handler from `ProgressTracker::ProgressCompleted`, the end of
/// the load step, so a navigation asked for before that point means no icon at all and one asked for
/// after it does not stop the download. Measured on stock Chrome 150.0.7871.46 over a loopback
/// server, with the page beaconing its own `load` and its own navigation into the same access log so
/// the two orders are READ:
///
/// ```text
/// never navigates                     /a.html  beacon-load  /favicon.ico
/// sync in the load handler            /a.html  beacon-load  beacon-go  /b.html  /declared.png
/// a microtask queued in the handler   /a.html  beacon-load  beacon-go  /b.html  /declared.png
/// setTimeout(..., 0)   in it          /a.html  beacon-load  beacon-go  /favicon.ico  /b.html  ...
/// setTimeout(..., 4)   in it          /a.html  beacon-load  beacon-go  /favicon.ico  /b.html  ...
/// setTimeout(..., 12)  in it          /a.html  beacon-load  /favicon.ico  beacon-go  /b.html  ...
/// a script navigates during parse     /a.html  beacon-go  /b.html  /declared.png   (no `load`)
/// ```
///
/// THIS HARNESS DOES NOT PERFORM THE NAVIGATION -- nothing here drives a top-level one; the engine
/// records the request and a driver would take it -- so `/b.html` and `/declared.png` are absent from
/// every row below and the assertion is what the LEFT document asked for, which is the whole
/// question. `beacon.png` stands in for Chrome's `beacon-load`, and its presence is what keeps an
/// empty log from passing for the wrong reason.
#[tokio::test]
async fn a_document_a_script_leaves_asks_for_no_icon() {
    const BEACON: &str = "beacon.png";
    let go = |js: &str| {
        format!(
            "<script>addEventListener('load', function () {{ new Image().src = '{BEACON}'; {js} }})</script>"
        )
    };
    let rows: &[(&str, String, &[&str])] = &[
        ("never navigates", go(""), &["/beacon.png", "/favicon.ico"]),
        ("the load handler navigates synchronously", go("location.href = 'b.html';"), &["/beacon.png"]),
        (
            "a microtask queued in the load handler navigates",
            go("Promise.resolve().then(function () { location.href = 'b.html'; });"),
            &["/beacon.png"],
        ),
        (
            "a task queued in the load handler navigates",
            go("setTimeout(function () { location.href = 'b.html'; }, 0);"),
            &["/beacon.png", "/favicon.ico"],
        ),
        (
            "a javascript: URL is not a navigation",
            go("location.href = 'javascript:void 0';"),
            &["/beacon.png", "/favicon.ico"],
        ),
    ];
    for who in BUNDLES {
        for (label, body, want) in rows {
            let r = run(Who::Bundle(who), "", body, None).await;
            assert_log(label, who_name(Who::Bundle(who)), &r.requests, want);
        }
    }
}

// ---- what the PAGE's own document.write puts in the document ---------------------------------

/// `document.write()` on the page's own document used to insert nothing at all, so
/// nothing it wrote was ever fetched. Chrome 150 over the same markup on a loopback server (probe
/// `dwsub.html`) loads all three -- the script runs (`window.DW_RAN`), the sheet reaches
/// `document.styleSheets` and the image reports `complete` -- and korvus now matches on each.
#[tokio::test]
async fn what_the_page_writes_into_itself_loads() {
    let rows: &[(&str, &str, &str, &[&str])] = &[
        (
            "a written <script src> is fetched and runs",
            "",
            r#"<script>document.write('<scr' + 'ipt src="dw1.js"><\/scr' + 'ipt>');</script>"#,
            &["/dw1.js", "/dw1-exec.png", "/favicon.ico"],
        ),
        (
            "a written stylesheet and image are fetched",
            "",
            r#"<script>document.write('<link rel=stylesheet href=dw2.css><img src=dw2-img.png>');</script>"#,
            &["/dw2.css", "/dw2-img.png", "/favicon.ico"],
        ),
        (
            "two writes from one script both land",
            "",
            r#"<script>document.write('<img src=dw3a.png>');document.write('<img src=dw3b.png>');</script>"#,
            &["/dw3a.png", "/dw3b.png", "/favicon.ico"],
        ),
    ];
    for who in BUNDLES {
        for (label, head, body, want) in rows {
            check(Who::Bundle(who), label, head, body, None, want).await;
        }
    }
}

// ---- about:blank and srcdoc frames -----------------------------------------------------------

#[tokio::test]
async fn what_is_inserted_into_an_initial_frame_document_loads_there() {
    let rows: &[(&str, &str, &str, &[&str])] = &[
        (
            "script, image and stylesheet appended to an appended about:blank frame",
            "",
            r#"<script>var f=document.createElement("iframe");document.body.appendChild(f);var d=f.contentWindow.document;var s=d.createElement("script");s.src="ifr.js";d.head.appendChild(s);var im=d.createElement("img");im.src="ifr-img.png";d.body.appendChild(im);var l=d.createElement("link");l.rel="stylesheet";l.href="ifr.css";d.head.appendChild(l);</script>"#,
            &["/ifr.js", "/ifr.css", "/ifr-img.png", "/ifr-exec.png", "/favicon.ico"],
        ),
        (
            "script appended to a parsed about:blank frame's body",
            "",
            r#"<iframe id=f></iframe><script>var d=document.getElementById("f").contentDocument;var s=d.createElement("script");s.src="bp.js";d.body.appendChild(s);</script>"#,
            &["/bp.js", "/bp-exec.png", "/favicon.ico"],
        ),
        (
            "document.write of an image and a script",
            "",
            r#"<iframe id=f></iframe><script>var d=document.getElementById("f").contentDocument;d.open();d.write("<img src=bw-img.png><scr"+"ipt src=bw.js></scr"+"ipt>");d.close();</script>"#,
            &["/bw.js", "/bw-img.png", "/bw-exec.png", "/favicon.ico"],
        ),
        (
            "script and image appended to a srcdoc frame on its load",
            "",
            r#"<iframe id=f srcdoc="<p>x"></iframe><script>var f=document.getElementById("f");f.onload=function(){var d=f.contentDocument;var s=d.createElement("script");s.src="sd.js";d.head.appendChild(s);var im=d.createElement("img");im.src="sd-img.png";d.body.appendChild(im);};</script>"#,
            &["/sd.js", "/sd-img.png", "/sd-exec.png", "/favicon.ico"],
        ),
        (
            "the parent's <base> is the blank frame's base",
            r#"<base href="/sub/">"#,
            r#"<script>var f=document.createElement("iframe");document.body.appendChild(f);var d=f.contentDocument;var s=d.createElement("script");s.src="bb.js";d.head.appendChild(s);</script>"#,
            &["/sub/bb.js", "/sub/bb-exec.png", "/favicon.ico"],
        ),
    ];
    for who in BUNDLES {
        for (label, head, body, want) in rows {
            check(Who::Bundle(who), label, head, body, None, want).await;
        }
    }
}

// ---- srcset and <picture> --------------------------------------------------------------------

const SRCSET_X: &str = r#"<img srcset="sx-1x.png 1x, sx-2x.png 2x"><img src=sy-src.png srcset="sy-2x.png 2x"><img src=sz-src.png srcset="sz-15.png 1.5x"><img srcset="sa-15.png 1.5x, sa-4.png 4x"><img srcset="sb-1.png, sb-25.png 2.5x"><img srcset="sc-05.png 0.5x, sc-15.png 1.5x"><img srcset="sd-08.png 0.8x, sd-3.png 3x"><img srcset="se-3.png 3x, se-1.png 1x"><img srcset="sf-09.png 0.9x, sf-11.png 1.1x"><img srcset="sg-a.png 1x, sg-b.png 1x">"#;
const SRCSET_W: &str = r#"<img srcset="w-400.png 400w, w-1600.png 1600w" sizes=100px><img src=hh-src.jpg sizes="(max-width: 479px) 85vw, (max-width: 767px) 63vw, (max-width: 991px) 33vw, 32vw" srcset="hh-500.jpg 500w, hh-800.jpg 800w, hh-1080.jpg 1080w, hh-1600.jpg 1600w, hh-2000.jpg 2000w"><img srcset="nz-1000.png 1000w, nz-2000.png 2000w, nz-4000.png 4000w"><img sizes="calc(50vw - 10px)" srcset="cz-900.png 900w, cz-1000.png 1000w"><img sizes="10em" srcset="em-100.png 100w, em-170.png 170w"><img sizes="(min-width: 5000px) 10px, 300px" srcset="mc-280.png 280w, mc-310.png 310w"><img sizes="bogus, 200px" srcset="bg-190.png 190w, bg-210.png 210w">"#;
const SRCSET_SCRIPT: &str = r#"<script>
var a=document.createElement("img");a.src="ia-a.png";a.srcset="ia-b.png 2x";document.body.appendChild(a);
var b=new Image();b.src="ib-a.png";b.srcset="ib-b.png 1x";
var c=new Image();c.src="ic-a.png";setTimeout(function(){c.srcset="ic-b.png 1x"},300);
var d=new Image();d.srcset="id-a.png 1x";setTimeout(function(){d.srcset="id-b.png 1x"},300);
var e=new Image();e.src="ie-a.png";setTimeout(function(){e.srcset="ie-b.png 2x"},300);
var g=new Image();g.srcset="ig-a.png 1x";setTimeout(function(){g.sizes="50px"},300);
var h=new Image();h.srcset="ih-100.png 100w, ih-400.png 400w";h.sizes="100px";setTimeout(function(){h.sizes="300px"},300);
</script>"#;
const PICTURE: &str = r#"<picture><source media="(min-width: 800px)" srcset=pm-wide.png><source srcset=pm-narrow.png><img src=pm-img.png></picture><picture><source media="(max-width: 800px)" srcset=pn-narrow.png><img src=pn-img.png></picture><picture><source sizes=100px srcset="pw-90.png 90w, pw-110.png 110w"><img src=pw-img.png></picture><picture><source srcset="px-1.png 1x, px-2.png 2x"><img src=px-img.png srcset="px-imgss.png"></picture><picture><source type="image/webp" media="(prefers-color-scheme: dark)" srcset=pd-dark.webp><img src=pd-img.png></picture>"#;

#[tokio::test]
async fn srcset_and_picture_pick_chrome_s_candidate_at_the_persona_s_dpr() {
    let at1: &[(&str, &str, &[&str])] = &[
        ("x descriptors", SRCSET_X, &["/sx-1x.png", "/sy-src.png", "/sz-src.png", "/sa-15.png", "/sb-1.png", "/sc-15.png", "/sd-3.png", "/se-1.png", "/sf-11.png", "/sg-a.png", "/favicon.ico"]),
        ("w descriptors and sizes", SRCSET_W, &["/w-400.png", "/hh-800.jpg", "/nz-2000.png", "/cz-1000.png", "/em-170.png", "/mc-310.png", "/bg-210.png", "/favicon.ico"]),
        ("srcset and sizes set from script", SRCSET_SCRIPT, &["/ia-a.png", "/ib-b.png", "/ic-a.png", "/ic-b.png", "/id-a.png", "/id-b.png", "/ie-a.png", "/ig-a.png", "/ih-100.png", "/ih-400.png", "/favicon.ico"]),
        ("picture", PICTURE, &["/pm-wide.png", "/pn-img.png", "/pw-110.png", "/px-1.png", "/pd-img.png", "/favicon.ico"]),
    ];
    for who in BUNDLES {
        for (label, body, want) in at1 {
            check(Who::Bundle(who), label, "", body, None, want).await;
        }
    }
    let at2: &[(&str, &str, &[&str])] = &[
        ("x descriptors", SRCSET_X, &["/sx-2x.png", "/sy-2x.png", "/sz-15.png", "/sa-4.png", "/sb-25.png", "/sc-15.png", "/sd-3.png", "/se-3.png", "/sf-11.png", "/sg-a.png", "/favicon.ico"]),
        ("w descriptors and sizes", SRCSET_W, &["/w-400.png", "/hh-1600.jpg", "/nz-4000.png", "/cz-1000.png", "/em-170.png", "/mc-310.png", "/bg-210.png", "/favicon.ico"]),
        ("srcset and sizes set from script", SRCSET_SCRIPT, &["/ia-b.png", "/ib-b.png", "/ic-a.png", "/ic-b.png", "/id-a.png", "/id-b.png", "/ie-a.png", "/ie-b.png", "/ig-a.png", "/ih-400.png", "/favicon.ico"]),
        ("picture", PICTURE, &["/pm-wide.png", "/pn-img.png", "/pw-110.png", "/px-2.png", "/pd-img.png", "/favicon.ico"]),
    ];
    for (label, body, want) in at2 {
        check(Who::Dpr2, label, "", body, None, want).await;
    }
}

// ---- loading=lazy ----------------------------------------------------------------------------

/// Absolutely positioned 10x10 lazy elements, `step` apart, inside a relative box.
fn grid(tag: &str, prefix: &str, lo: u32, hi: u32, step: u32, horizontal: bool) -> (String, Vec<(u32, String)>) {
    let mut html = String::new();
    let mut paths = Vec::new();
    for off in (lo..=hi).step_by(step as usize) {
        let pos = if horizontal { format!("left:{off}px;top:0") } else { format!("top:{off}px;left:0") };
        let path = format!("/{prefix}{off}.{}", if tag == "img" { "png" } else { "html" });
        if tag == "img" {
            html.push_str(&format!(r#"<img loading=lazy style="position:absolute;{pos};width:10px;height:10px" src="{}">"#, &path[1..]));
        } else {
            html.push_str(&format!(r#"<iframe loading=lazy style="position:absolute;{pos};width:10px;height:10px;border:0" src="{}"></iframe>"#, &path[1..]));
        }
        paths.push((off, path));
    }
    let size = if horizontal { format!("width:{}px;height:10px", hi + 2000) } else { format!("height:{}px", hi + 2000) };
    (format!(r#"<div style="position:relative;{size}">{html}</div>"#), paths)
}

#[tokio::test]
async fn lazy_elements_load_within_chrome_s_distance_of_the_viewport() {
    // The body's default 8px margin puts every grid element 8px further from the page's origin.
    const BODY: f64 = 8.0;
    for who in BUNDLES {
        let name = who_name(Who::Bundle(who));
        for (tag, prefix, lo, hi, step, horizontal, margin) in [
            ("img", "img-v", 1900u32, 2600u32, 10u32, false, 1250.0),
            ("img", "img-h", 2300, 2800, 10, true, 625.0),
            ("iframe", "iframe-v", 3200, 3700, 20, false, 2500.0),
        ] {
            let (body, paths) = grid(tag, prefix, lo, hi, step, horizontal);
            let r = run(Who::Bundle(who), "", &body, None).await;
            let extent = if horizontal { r.inner.0 } else { r.inner.1 };
            let mut want: Vec<&str> = paths.iter().filter(|(off, _)| f64::from(*off) + BODY < extent + margin).map(|(_, p)| p.as_str()).collect();
            want.push("/favicon.ico");
            assert!(want.len() > 1 && want.len() < paths.len() + 1, "{prefix} under {name}: the grid must straddle the threshold (viewport {extent})");
            assert_log(&format!("{tag} grid {prefix} (viewport {extent}, margin {margin})"), name, &r.requests, &want);
        }
    }
}

#[tokio::test]
async fn lazy_loading_follows_display_scrolling_and_scroll_containers() {
    let misc = r#"<div style="height:12000px;position:relative">
<img loading=lazy src=lm-inview.png width=10 height=10>
<img loading=lazy src=lm-hidden-top.png style="display:none">
<img loading=lazy src=lm-vis-hidden.png style="visibility:hidden;width:10px;height:10px">
<img loading=lazy src=lm-nosize-far.png style="position:absolute;top:8000px">
<img loading=LAZY src=lm-upper-far.png style="position:absolute;top:8000px;width:10px;height:10px">
<img loading=eager src=lm-eager-far.png style="position:absolute;top:8000px;width:10px;height:10px">
<img loading=bogus src=lm-bogus-far.png style="position:absolute;top:8000px;width:10px;height:10px">
<div style="display:none"><img loading=lazy src=lm-in-hidden-parent.png width=10 height=10></div>
<img loading=lazy srcset="lm-srcset-far.png 1x" style="position:absolute;top:8000px;width:10px;height:10px">
<picture><source srcset=lm-pic-far.png><img loading=lazy src=lm-picimg-far.png style="position:absolute;top:8000px;width:10px;height:10px"></picture>
</div>
<script>var i=new Image();i.loading="lazy";i.src="lm-detached.png";
var j=document.createElement("img");j.src="lm-script-far.png";j.loading="lazy";j.style.cssText="position:absolute;top:8000px;width:10px;height:10px";document.body.firstElementChild.appendChild(j);
var k=document.createElement("img");k.loading="lazy";k.style.cssText="position:absolute;top:8000px;width:10px;height:10px";document.body.firstElementChild.appendChild(k);k.src="lm-script2-far.png";
setTimeout(function(){var m=document.querySelector('img[src="lm-eager-far.png"]');m.loading="lazy";m.src="lm-eager-then-lazy-far.png";},300);
</script>"#;
    let scroll = r#"<div style="height:12000px;position:relative">
<img loading=lazy src=ls-3000.png style="position:absolute;top:3000px;width:10px;height:10px">
<img loading=lazy src=ls-6000.png style="position:absolute;top:6000px;width:10px;height:10px">
<img loading=lazy src=ls-9000.png style="position:absolute;top:9000px;width:10px;height:10px">
<iframe loading=lazy src=ls-6000.html style="position:absolute;top:6000px;left:50px;width:10px;height:10px"></iframe>
</div>"#;
    let flow = (0..12).map(|i| format!(r#"<div style="height:400px">block {i}</div><img loading=lazy src="fl-{i}.png" width=300 height=200>"#)).collect::<String>();
    let flow = format!("<p>intro</p>{flow}");
    // A 500x300 scroller: images 1250px below / 625px right of ITS edge load, frames 2500px.
    let clip_h = (900..=1300).step_by(20).map(|o| format!(r#"<img loading=lazy src="sch-{o}.png" style="position:absolute;left:{o}px;top:0;width:10px;height:10px">"#)).collect::<String>();
    let clip_h = format!(r#"<div style="overflow:auto;width:500px;height:300px;position:relative"><div style="position:relative;width:2300px;height:10px">{clip_h}</div></div>"#);
    let clip_v = (1300..1800).step_by(50).map(|o| format!(r#"<img loading=lazy src="hc-v{o}.png" style="position:absolute;top:{o}px;left:0;width:10px;height:10px">"#)).collect::<String>();
    let clip_v = format!(r#"<div style="overflow:hidden;width:500px;height:300px;position:relative"><div style="position:relative;width:4000px;height:3000px">{clip_v}</div></div>"#);
    let frames_v = (2000..=3000).step_by(50).map(|o| format!(r#"<iframe loading=lazy src="scv-{o}.html" style="position:absolute;top:{o}px;left:0;width:10px;height:10px;border:0"></iframe>"#)).collect::<String>();
    let frames_v = format!(r#"<div style="overflow:auto;width:500px;height:300px;position:relative"><div style="position:relative;height:4000px;width:10px">{frames_v}</div></div>"#);
    let carousel = r#"<div style="position:relative;width:500px;height:200px">
<img id=s1 loading=lazy src=cs-1.png style="position:absolute;width:100%;height:200px;display:block">
<img id=s2 loading=lazy src=cs-2.png style="position:absolute;width:100%;height:200px;display:none">
<img id=s3 loading=lazy src=cs-3.png style="position:absolute;width:100%;height:200px;display:none">
</div>"#;
    let slides = (0..6).map(|i| format!(r#"<div style="flex:0 0 1000px"><img loading=lazy src="tr-{i}.png" style="width:100%;height:100px"></div>"#)).collect::<String>();
    let track = format!(r#"<div style="overflow:hidden;width:1000px;margin-left:100px"><div style="display:flex;transform:translateX(-2000px)">{slides}</div></div>"#);
    let owned = |v: Vec<String>| v;
    let sch: Vec<String> = owned((900..=1120).step_by(20).map(|o| format!("/sch-{o}.png")).collect());
    let hcv: Vec<String> = (1300..=1500).step_by(50).map(|o| format!("/hc-v{o}.png")).collect();
    let scv: Vec<String> = (2000..=2750).step_by(50).map(|o| format!("/scv-{o}.html")).collect();
    for who in BUNDLES {
        let w = Who::Bundle(who);
        check(w, "display, visibility, case, eager, detached, script-set", "", misc, None, &["/lm-inview.png", "/lm-vis-hidden.png", "/lm-eager-far.png", "/lm-bogus-far.png", "/favicon.ico"]).await;
        check(w, "scrollTo(0, 6000)", "", scroll, Some("scrollTo(0, 6000)"), &["/ls-6000.png", "/ls-6000.html", "/favicon.ico"]).await;
        check(w, "normal flow", "", &flow, None, &["/fl-0.png", "/fl-1.png", "/fl-2.png", "/favicon.ico"]).await;
        let mut want: Vec<&str> = sch.iter().map(String::as_str).collect();
        want.push("/favicon.ico");
        check(w, "images right of an overflow:auto box", "", &clip_h, None, &want).await;
        let mut want: Vec<&str> = hcv.iter().map(String::as_str).collect();
        want.push("/favicon.ico");
        check(w, "images below an overflow:hidden box", "", &clip_v, None, &want).await;
        let mut want: Vec<&str> = scv.iter().map(String::as_str).collect();
        want.push("/favicon.ico");
        check(w, "frames below an overflow:auto box", "", &frames_v, None, &want).await;
        // a production site's banner: a slide's lazy image waits while the slide is `display: none` and loads when a style
        // write shows it; a Swiper track moved by a `transform` inside its `overflow: hidden` box brings its slides near
        // where the transform puts them -- the one in view and its neighbours within 625px of the box's two edges
        // (Chrome 150, 2026-10-24: `cs-1 cs-2 tr-1 tr-2 tr-3`)
        check(w, "a carousel slide shown by a style write", "", carousel, Some(r#"document.getElementById("s2").style.display = "block""#), &["/cs-1.png", "/cs-2.png", "/favicon.ico"]).await;
        check(w, "a transformed carousel track", "", &track, None, &["/tr-1.png", "/tr-2.png", "/tr-3.png", "/favicon.ico"]).await;
    }
}

// ---- one request per image URL ---------------------------------------------------------------

/// Chrome requests an image URL ONCE per document however many things name it -- elements, a later
/// `new Image()`, a CSS background, a `srcset` candidate (its memory cache, even under `no-store`).
/// The favicon is the browser's own request and is not shared with the page's.
#[tokio::test]
async fn an_image_url_is_requested_once_per_document() {
    let rows: &[(&str, &str, &str, &[&str])] = &[
        ("two <img> and a later new Image()", "", r#"<img src=dup1.png><img src=dup1.png><script>setTimeout(function(){var i=new Image();i.src="dup1.png";},300)</script>"#, &["/dup1.png", "/favicon.ico"]),
        ("<img> and a CSS background", "<style>.bg{background:url(dup2.png);width:10px;height:10px}</style>", "<img src=dup2.png><div class=bg></div>", &["/dup2.png", "/favicon.ico"]),
        ("a CSS background, then new Image()", "<style>.bg{background:url(dup3.png);width:10px;height:10px}</style>", r#"<div class=bg></div><script>setTimeout(function(){var i=new Image();i.src="dup3.png";},300)</script>"#, &["/dup3.png", "/favicon.ico"]),
        ("a srcset candidate and a src", "", r#"<img srcset="dup4.png 1x"><img src=dup4.png>"#, &["/dup4.png", "/favicon.ico"]),
        ("a far lazy <img> and a CSS background", "<style>.bg{background:url(dup5.png);width:10px;height:10px}</style>", r#"<div class=bg></div><img loading=lazy src=dup5.png style="position:absolute;top:9000px">"#, &["/dup5.png", "/favicon.ico"]),
        ("an icon and an <img>", "<link rel=icon href=dup6.png>", "<img src=dup6.png>", &["/dup6.png", "/dup6.png"]),
    ];
    for who in BUNDLES {
        for (label, head, body, want) in rows {
            let r = run(Who::Bundle(who), head, body, None).await;
            let mut want_counts: BTreeMap<String, usize> = BTreeMap::new();
            for p in *want {
                *want_counts.entry(p.to_string()).or_default() += 1;
            }
            assert_eq!(counts(&r.requests), want_counts, "{label} under {who}: the access log differs from Chrome's");
        }
    }
}

// ---- one request per URL while that request is in flight -------------------------

/// A LOAD JOINS THE REQUEST THIS DOCUMENT ALREADY HAS IN FLIGHT FOR THE SAME URL, whatever asks and
/// whatever the response will say. Blink's memory cache is one mechanism with two halves and this
/// is the first: in flight, the join is unconditional; settled, reuse depends on HTTP freshness,
/// and this engine has no HTTP cache so it does not claim that half.
///
/// EVERY ROW IS STOCK CHROME 150.0.7871.46's ACCESS LOG for the same markup, over a loopback server
/// answering `Cache-Control: no-store` -- the same server these fixtures use, so nothing in the
/// measurement or in the assertion can be reusing a cached response:
///
/// ```text
///   three <link rel=stylesheet> naming one file      chrome 1   korvus was 3
///   three <link rel=prefetch>   naming one file      chrome 1   korvus was 3
///   five  <script src> appended in one loop          chrome 1   korvus was 5
///   eight concurrent import() of one module          chrome 1   korvus was 8
///   five  await import() of one module, in series    chrome 1   korvus 1
///   two   @font-face naming one .woff2               chrome 1   korvus 1
///   five  fetch() of one url                         chrome 5   korvus 5
///   three parser <script src> naming one file        chrome 3   korvus 3
/// ```
///
/// THE LAST TWO ROWS ARE THE ONES THAT KEEP THIS HONEST, because each names a place the rule STOPS.
/// A `fetch()` is not a memory-cache client and Chrome puts all five on the wire; sharing them
/// would be a fix that moved us away from Chrome. And three parser `<script src>` are three
/// requests in CHROME under `no-store` -- measured, not reasoned about, and the SETTLED half of
/// the cache standing in the open: Chrome answers 1 for the same markup under `Cache-Control:
/// max-age=300`, also measured, and so would we if we had a cache.
#[tokio::test]
async fn a_load_joins_the_request_this_document_already_has() {
    // (label, head, body, the access log -- exactly, with multiplicity)
    let rows: &[(&str, &str, &str, &[&str])] = &[
        (
            "three <link rel=stylesheet> naming one file",
            "<link rel=stylesheet href=j1.css><link rel=stylesheet href=j1.css><link rel=stylesheet href=j1.css>",
            "",
            &["/favicon.ico", "/j1.css"],
        ),
        (
            "three <link rel=prefetch> naming one file",
            "<link rel=prefetch href=j2.js><link rel=prefetch href=j2.js><link rel=prefetch href=j2.js>",
            "",
            &["/favicon.ico", "/j2.js"],
        ),
        (
            "five <script src> a script appended in one loop",
            "",
            r#"<script>for(var i=0;i<5;i++){var s=document.createElement("script");s.src="j3.js";
               document.head.appendChild(s);}</script>"#,
            // One fetch, FIVE evaluations: each element still runs the shared response, and the five
            // images they create are one request for the same reason this test exists.
            &["/favicon.ico", "/j3.js", "/j3-exec.png"],
        ),
        (
            "eight concurrent import() of one module",
            "",
            r#"<script type=module>for(let i=0;i<8;i++)import("./j4.mjs").catch(function(){});</script>"#,
            &["/favicon.ico", "/j4.mjs"],
        ),
        (
            "five await import() of one module, in series",
            "",
            r#"<script type=module>(async function(){for(let i=0;i<5;i++)await import("./j5.mjs");})();</script>"#,
            &["/favicon.ico", "/j5.mjs"],
        ),
        (
            "two @font-face rules naming one file",
            "<style>@font-face{font-family:fa;src:url(j6.woff2)}@font-face{font-family:fb;src:url(j6.woff2)}\
             p{font-family:fa}i{font-family:fb}</style>",
            "<p>a</p><i>b</i>",
            &["/favicon.ico", "/j6.woff2"],
        ),
        (
            "five fetch() of one url -- NOT a memory-cache client",
            "",
            r#"<script>for(var i=0;i<5;i++)fetch("j7.json").catch(function(){});</script>"#,
            &["/favicon.ico", "/j7.json", "/j7.json", "/j7.json", "/j7.json", "/j7.json"],
        ),
        (
            "three parser <script src> naming one file: the settled half, which Chrome does not reuse either",
            "",
            "<script src=j8.js></script><script src=j8.js></script><script src=j8.js></script>",
            &["/favicon.ico", "/j8.js", "/j8.js", "/j8.js", "/j8-exec.png"],
        ),
    ];
    for who in BUNDLES {
        for (label, head, body, want) in rows {
            let r = run(Who::Bundle(who), head, body, None).await;
            let mut want_counts: BTreeMap<String, usize> = BTreeMap::new();
            for p in *want {
                *want_counts.entry(p.to_string()).or_default() += 1;
            }
            assert_eq!(counts(&r.requests), want_counts, "{label} under {who}: the access log differs from Chrome's");
        }
    }
}

// ---- an SVG <use> reference is a document plus a fragment ------------------------

/// `<use href>` NAMES A DOCUMENT AND A FRAGMENT OF IT, and only the document is a request. A
/// reference into THIS document is resolved in the tree that is already here and fetches nothing;
/// an external one fetches the resource document, with the fragment removed, and every `<use>` of
/// that document shares the one request.
///
/// STOCK CHROME 150.0.7871.46's ACCESS LOG, the same loopback server:
///
/// ```text
///   three <use href="#s"> into a <symbol> in this document   chrome 0   korvus was 3
///   <use href="#nope">, naming nothing at all                chrome 0   korvus was 1
///   three <use href="sp.svg#a|#b|#a">                        chrome 1   korvus was 3
///   two   <use href="sp.svg">, no fragment at all            chrome 1
/// ```
///
/// korvus put the WHOLE reference on the wire, fragment and all, once per element -- so a page's
/// own URL was requested again for every icon on it (`carto.com`: 49 times) and an icon sprite once
/// per `<use>` (`docs.nginx.com`: 187 times for one file).
#[tokio::test]
async fn an_svg_use_reference_is_a_document_plus_a_fragment() {
    let rows: &[(&str, &str, &str, &[&str])] = &[
        (
            "three <use> into a <symbol> in this document",
            "",
            r##"<svg style=display:none><symbol id=us viewBox="0 0 8 8"><rect width=8 height=8/></symbol></svg>
               <svg><use href="#us"/></svg><svg><use href="#us"/></svg><svg><use href="#us"/></svg>"##,
            &["/favicon.ico"],
        ),
        (
            "a <use> naming nothing at all",
            "",
            r##"<svg><use href="#nothing-here"/></svg>"##,
            &["/favicon.ico"],
        ),
        (
            "three <use> into one external sprite, two fragments",
            "",
            r##"<svg><use href="u1.svg#a"/></svg><svg><use xlink:href="u1.svg#b"/></svg>
               <svg><use href="u1.svg#a"/></svg>"##,
            &["/favicon.ico", "/u1.svg"],
        ),
        (
            "two <use> at an external document with no fragment",
            "",
            r#"<svg><use href="u2.svg"/></svg><svg><use href="u2.svg"/></svg>"#,
            &["/favicon.ico", "/u2.svg"],
        ),
    ];
    for who in BUNDLES {
        for (label, head, body, want) in rows {
            check(Who::Bundle(who), label, head, body, None, want).await;
        }
    }
}

// ---- script-inserted scripts: in order, or as they arrive ------------------------------------

/// Chrome runs script-inserted `<script src>` elements whose `async` was set to false IN INSERTION
/// ORDER, however late the first one arrives -- even if `async` is set back to true after insertion --
/// and force-async ones as they arrive. Each script requests `ran-<n>-<name>.png`, `n` its run order.
#[tokio::test]
async fn inserted_scripts_run_in_chrome_s_order() {
    let rows: &[(&str, &str, &[&str])] = &[
        (
            "async = false, the first 600 ms late",
            r#"<script>["slow-ord-o1.js","ord-o2.js"].forEach(function(u){var s=document.createElement("script");s.src=u;s.async=false;document.body.appendChild(s);});</script>"#,
            &["/slow-ord-o1.js", "/ord-o2.js", "/ran-1-slow-ord-o1.png", "/ran-2-ord-o2.png", "/favicon.ico"],
        ),
        (
            "async = false, then the first set async after insertion",
            r#"<script>var ss=["slow-ord-a1.js","ord-a2.js"].map(function(u){var s=document.createElement("script");s.src=u;s.async=false;document.body.appendChild(s);return s;});ss[0].async=true;</script>"#,
            &["/slow-ord-a1.js", "/ord-a2.js", "/ran-1-slow-ord-a1.png", "/ran-2-ord-a2.png", "/favicon.ico"],
        ),
        (
            "force-async: as they arrive",
            r#"<script>["slow-ord-f1.js","ord-f2.js"].forEach(function(u){var s=document.createElement("script");s.src=u;document.body.appendChild(s);});</script>"#,
            &["/slow-ord-f1.js", "/ord-f2.js", "/ran-1-ord-f2.png", "/ran-2-slow-ord-f1.png", "/favicon.ico"],
        ),
        (
            "async = false, inserted from onload",
            r#"<script>onload=function(){["slow-ord-m1.js","ord-m2.js"].forEach(function(u){var s=document.createElement("script");s.src=u;s.async=false;document.head.appendChild(s);});}</script>"#,
            &["/favicon.ico", "/slow-ord-m1.js", "/ord-m2.js", "/ran-1-slow-ord-m1.png", "/ran-2-ord-m2.png"],
        ),
    ];
    for who in BUNDLES {
        for (label, body, want) in rows {
            check(Who::Bundle(who), label, "", body, None, want).await;
        }
    }
}

// ---- the SCRIPT TYPE step, which is upstream of the fetch -----------------------

/// A `<script src>` whose `type` names no script type is NEVER REQUESTED, whichever seam inserted
/// it -- HTML's "prepare the script element" determines the script block's type and returns when it
/// is null, three steps before the fetch, exactly as `nomodule` does.
///
/// WHY THIS IS NOT AN EDGE CASE, AND WHY THE FIRST ROW IS THE ONE THAT MATTERS. It is how jQuery
/// loads a script, so it is how a large share of the web loads one. `$("<script>").attr("src", u)
/// .appendTo("head")` reaches `domManip`, which NEUTRALISES the element before inserting it
/// (`disableScript`: `elem.type = (elem.getAttribute("type") !== null) + "/" + elem.type`), runs
/// the URL itself through `_evalUrl`, and restores `type` afterwards -- a write that re-prepares
/// nothing, so the element stays inert forever.
///
/// MEASURED ON STOCK CHROME 150, the replayed `docs.oracle.com/en/java` recording, with
/// `Node.prototype.appendChild` and the `type` setter hooked at document-start:
///
/// ```text
///   104 ms  appendChild  type=null      src=.../oracle-universal.js  intoDoc=false
///   104 ms  script.type= "false/"       src=.../oracle-universal.js
///   104 ms  appendChild  type="false/"  src=.../oracle-universal.js  intoDoc=TRUE
/// ```
///
/// Chrome requested that file ONCE and ran it once. korvus requested it TWICE and ran it twice,
/// so two copies of a 147 KB component held two closures over one DOM: the page read 1,260 nodes
/// or 386 on identical bytes, flipping inside a single process.
///
/// The rule is asserted over the ATTRIBUTE, not over jQuery: the negative rows are the shapes that
/// have no script type (jQuery's two, a data type, a bare invalid spelling, whitespace-only) and
/// the positive rows are the shapes that do, including the pair `type=""` / `type=" "` that tells
/// a raw-empty test from a stripped one. `counts` and not membership, so a DOUBLE request fails
/// too -- that is the defect this exists for.
#[tokio::test]
async fn a_script_with_no_script_type_is_never_requested() {
    // (label, the js insertion, the paths the access log must hold -- exactly, with multiplicity)
    let rows: &[(&str, &str, &[&str])] = &[
        (
            "jQuery's own shape: disableScript, insert, restore",
            r#"<script>var s=document.createElement("script");s.src="ty-jq.js";
               s.type=(s.getAttribute("type")!==null)+"/"+s.type;document.head.appendChild(s);
               s.type="text/javascript";</script>"#,
            &["/favicon.ico"],
        ),
        (
            "disableScript over an existing type",
            r#"<script>var s=document.createElement("script");s.src="ty-jq2.js";
               s.type="false/text/javascript";document.head.appendChild(s);</script>"#,
            &["/favicon.ico"],
        ),
        (
            "a data type is data, not code",
            r#"<script>var s=document.createElement("script");s.src="ty-json.js";
               s.type="application/json";document.head.appendChild(s);</script>"#,
            &["/favicon.ico"],
        ),
        (
            "`javascript` unprefixed is not a MIME type",
            r#"<script>var s=document.createElement("script");s.src="ty-bare.js";
               s.type="javascript";document.head.appendChild(s);</script>"#,
            &["/favicon.ico"],
        ),
        (
            "whitespace-only strips to empty and is still NOT a type",
            r#"<script>var s=document.createElement("script");s.src="ty-ws.js";
               s.setAttribute("type"," ");document.head.appendChild(s);</script>"#,
            &["/favicon.ico"],
        ),
        (
            "language=vbscript, no type",
            r#"<script>var s=document.createElement("script");s.src="ty-vb.js";
               s.setAttribute("language","vbscript");document.head.appendChild(s);</script>"#,
            &["/favicon.ico"],
        ),
        (
            "no type at all: requested and run",
            r#"<script>var s=document.createElement("script");s.src="ty-plain.js";
               document.head.appendChild(s);</script>"#,
            &["/favicon.ico", "/ty-plain.js", "/ty-plain-exec.png"],
        ),
        (
            "type=\"\" is a classic script",
            r#"<script>var s=document.createElement("script");s.src="ty-empty.js";
               s.setAttribute("type","");document.head.appendChild(s);</script>"#,
            &["/favicon.ico", "/ty-empty.js", "/ty-empty-exec.png"],
        ),
        (
            "a JavaScript MIME type, uppercase and padded",
            r#"<script>var s=document.createElement("script");s.src="ty-mime.js";
               s.setAttribute("type"," TEXT/JAVASCRIPT ");document.head.appendChild(s);</script>"#,
            &["/favicon.ico", "/ty-mime.js", "/ty-mime-exec.png"],
        ),
        (
            "application/ecmascript is valid and does not contain \"javascript\"",
            r#"<script>var s=document.createElement("script");s.src="ty-ecma.js";
               s.setAttribute("type","application/ecmascript");document.head.appendChild(s);</script>"#,
            &["/favicon.ico", "/ty-ecma.js", "/ty-ecma-exec.png"],
        ),
        (
            "language=javascript, no type",
            r#"<script>var s=document.createElement("script");s.src="ty-lang.js";
               s.setAttribute("language","javascript");document.head.appendChild(s);</script>"#,
            &["/favicon.ico", "/ty-lang.js", "/ty-lang-exec.png"],
        ),
    ];
    for who in BUNDLES {
        for (label, body, want) in rows {
            let r = run(Who::Bundle(who), "", body, None).await;
            let mut want_counts: BTreeMap<String, usize> = BTreeMap::new();
            for p in *want {
                *want_counts.entry(p.to_string()).or_default() += 1;
            }
            assert_eq!(counts(&r.requests), want_counts, "{label} under {who}: the access log differs from Chrome's");
        }
    }
}

/// A LAZY CHECK WHILE FRAMES ARE RUNNING requests what Chrome requests: lazy images inserted under a page
/// with an observer and a running animation-frame loop load by the measured distance rule (1250px past the
/// viewport's bottom edge, [`lazy_elements_load_within_chrome_s_distance_of_the_viewport`]), again after a
/// later scroll, and a lazy frame inserted in the same circumstances loads too.
#[tokio::test]
async fn a_lazy_check_that_rides_a_frame_requests_what_chrome_does() {
    let body = r#"<div id=o style=height:10px></div><div id=c style="height:12000px;position:relative"></div>
<script>new IntersectionObserver(function () {}).observe(document.getElementById('o'));
var t0 = performance.now(); (function f() { if (performance.now() - t0 < 4000) requestAnimationFrame(f); })();</script>"#;
    let imgs = r#"var c = document.getElementById('c');
[100, 6000, 9000].forEach(function (y) { var i = document.createElement('img'); i.loading = 'lazy';
  i.style.cssText = 'position:absolute;left:0;top:' + y + 'px;width:10px;height:10px'; i.src = 'lz-' + y + '.png'; c.appendChild(i); });"#;
    let scrolled = format!("{imgs} setTimeout(function () {{ scrollTo(0, 5000); }}, 200);");
    let frame = r#"var f = document.createElement('iframe'); f.loading = 'lazy'; f.src = 'lz-f.html';
f.style.cssText = 'position:absolute;left:0;top:200px;width:10px;height:10px;border:0'; document.getElementById('c').appendChild(f);"#;
    for who in BUNDLES {
        let w = Who::Bundle(who);
        check(w, "lazy images inserted while frames run", "", body, Some(imgs), &["/lz-100.png", "/favicon.ico"]).await;
        check(w, "the same, then scrollTo(0, 5000)", "", body, Some(&scrolled), &["/lz-100.png", "/lz-6000.png", "/favicon.ico"]).await;
        check(w, "a lazy frame inserted while frames run", "", body, Some(frame), &["/lz-f.html", "/favicon.ico"]).await;
    }
}
