//! The window surface a bot check reads, against the four stock browsers of the family.
//!
//! # The oracle
//!
//! `fixtures/p11a/probe.js` is evaluated UNMODIFIED here and in each browser, section by section.
//! `fixtures/p11a/record.py` launches the stock binary itself (`--remote-debugging-port`, a throwaway
//! profile, never Playwright's launcher), serves the same routes this file's server does on two
//! loopback origins, and writes `<browser>.<section>.txt`. A line that differs here is a finding about
//! the engine, never a reason to edit a `.txt`.

use engine::Engine;

async fn page(persona: persona::CapturedPersona) -> Engine {
    let mut e = Engine::new(persona::worn_on_host(persona));
    e.run().await.expect("engine boot");
    e.load_html("<!doctype html><html><head></head><body></body></html>").expect("load_html");
    e.run().await.expect("run");
    e
}

/// EVERY INTERFACE OBJECT ON THE WINDOW IS NON-ENUMERABLE, under every bundle in the tree.
///
/// Every writer that published one used a plain `global.set`, which makes an enumerable property, so
/// `Object.keys(window)` listed ~880 interface objects where a browser lists none of them. Measured
/// over a loopback page on Chrome 150.0.7871.46, Brave 151.1.93.138, Edge 151.0.4129.93 and Opera
/// 135.0.5973.133: every interface object (a function with an own `prototype`) on `window` is
/// `{writable: true, enumerable: false, configurable: true}`, in all four, with no exception; the
/// window's own METHODS (`alert`, `fetch`, `setTimeout` …) are enumerable, and have no `prototype`.
#[tokio::test]
async fn no_interface_object_on_the_window_is_enumerable() {
    for id in bundle_ids() {
        let mut e = page(persona::CapturedPersona::select(Some(&id)).unwrap()).await;
        let v = e
            .eval_value(
                "p11a-enum",
                r#"JSON.stringify((() => {
                    const bad = [];
                    let ifaces = 0;
                    for (const k of Object.getOwnPropertyNames(globalThis)) {
                        const d = Object.getOwnPropertyDescriptor(globalThis, k);
                        if (!('value' in d) || typeof d.value !== 'function') continue;
                        if (!Object.prototype.hasOwnProperty.call(d.value, 'prototype')) continue;
                        ifaces++;
                        if (d.enumerable || !d.writable || !d.configurable) bad.push(k);
                    }
                    return { ifaces, bad };
                })())"#
                    .into(),
            )
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(v.as_str().unwrap()).unwrap();
        assert!(v["ifaces"].as_u64().unwrap() >= 800, "{id}: the window carries {} interface objects; the probe is vacuous", v["ifaces"]);
        assert_eq!(v["bad"], serde_json::json!([]), "{id}: interface objects that are not {{w: true, e: false, c: true}}");
    }
}

/// Interfaces the bundle records as an ALIAS of another interface object (`ctorName` differs from the
/// name) -- `WebKitCSSMatrix` -> `DOMMatrix`, `WebKitMutationObserver` -> `MutationObserver`.
fn legacy_aliases(id: &str) -> Vec<String> {
    let path = persona::CapturedPersona::select(Some(id)).unwrap().artifact("surface");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    v["interfaces"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(k, i)| i["ctorName"].as_str().map(|c| c != k.as_str()).unwrap_or(false))
        .map(|(k, _)| k.clone())
        .collect()
}

fn bundle_ids() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/korvus-persona/bundles");
    let mut ids: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|d| d.ok())
        .filter(|d| d.path().join("manifest.toml").is_file())
        .map(|d| d.file_name().to_string_lossy().to_string())
        .collect();
    ids.sort();
    assert!(!ids.is_empty(), "no persona bundle in the tree");
    ids
}

/// Every OWN property descriptor of `window`, `Window.prototype`, the named-properties object,
/// `document`, `location` and `Location.prototype` has the shape the bundle's own browser gives it, for every name both install -- under
/// all four bundles, each against its own browser's recording.
///
/// Earlier (release binary at e8b50144), against Chrome 150: 883 interface objects
/// enumerable, and 15 singletons an own DATA property where every browser has an accessor --
/// `document window self top parent frames location navigator screen performance history
/// localStorage sessionStorage customElements cookieStore` -- plus `origin` without its
/// `[Replaceable]` setter.
#[tokio::test]
async fn the_window_descriptors_are_the_browsers() {
    for id in bundle_ids() {
        let browser = browser_of(&id);
        let (mut e, same, xo) = served_page(&id).await;
        let got = run_section(&mut e, &same, &xo, "desc").await;
        // A LEGACY ALIAS is the one row not compared: the capture records `WebKitMutationObserver` as
        // an interface whose constructor is ANOTHER interface's (`ctorName` "MutationObserver"), and
        // this engine still builds it as its own generated shell, because `MutationObserver` is built
        // by a hand table long after the registry walk that creates the alias at its position. Derived
        // from the bundle, not listed, and reported as a remaining difference by P11a.
        let aliases = legacy_aliases(&id);
        let in_scope = |k: &str| {
            (k.starts_with("window.")
                || k.starts_with("Window.prototype.")
                || k.starts_with("WindowProperties.")
                || k.starts_with("document.")
                || k.starts_with("location.")
                || k.starts_with("Location.prototype.")
                || k.starts_with("Document.prototype.startViewTransition")
                || k.starts_with("ViewTransition.prototype."))
                && !aliases.iter().any(|a| k == format!("window.{a}"))
        };
        let shared = keyed(&got).keys().filter(|k| in_scope(k) && keyed(&fixture(&browser, "desc")).contains_key(*k)).count();
        assert!(shared >= 1000, "{id}: only {shared} descriptor keys in common with {browser}; the comparison is vacuous");
        let (only_browser, only_korvus) = assert_shared_keys(&format!("{id} desc"), &got, &fixture(&browser, "desc"), in_scope);
        assert!(only_browser.iter().chain(only_korvus.iter()).all(|k| is_window_global_key(k)), "{id}: a one-sided key that is not a window global: {only_browser:?} {only_korvus:?}");
        eprintln!("{id}: {shared} descriptors equal to {browser}'s; globals only {browser} has: {}; only korvus: {}", only_browser.len(), only_korvus.len());
    }
}

/// A foreign receiver throws `Illegal invocation` from every window singleton accessor and setter --
/// `Object.create(Window.prototype)` included -- and `get.call(undefined)` answers the global's.
/// `opener = x` replaces the accessor with a data property, `null` included; `delete window.navigator`
/// succeeds and `delete window.document` does not.
#[tokio::test]
async fn the_window_accessors_check_their_receiver_like_the_browsers() {
    for id in bundle_ids() {
        let browser = browser_of(&id);
        let (mut e, same, xo) = served_page(&id).await;
        let got = run_section(&mut e, &same, &xo, "recv").await;
        assert_lines(&format!("{id} recv"), &got, &fixture(&browser, "recv"));
    }
}

/// `window.X = 7` in sloppy code, for every own accessor of the window: a getter-only accessor keeps
/// its value, a `[Replaceable]` one becomes an own data property holding 7, a setter stores.
#[tokio::test]
async fn assigning_every_window_accessor_does_what_the_browsers_do() {
    for id in bundle_ids() {
        let browser = browser_of(&id);
        let (mut e, same, xo) = served_page(&id).await;
        let got = run_section(&mut e, &same, &xo, "replace").await;
        let (only_browser, _) = assert_shared_keys(&format!("{id} replace"), &got, &fixture(&browser, "replace"), |_| true);
        assert!(only_browser.iter().all(|k| is_window_global_key(k)), "{id}: {only_browser:?}");
    }
}


/// Every `Location` member's shape and argument handling -- the SyntaxError text for an unparseable
/// URL, the arity and receiver errors, `valueOf`/`@@toPrimitive`/`ancestorOrigins`, the immutable
/// prototype -- line for line against each bundle's own browser.
#[tokio::test]
async fn location_answers_like_the_browsers() {
    for id in bundle_ids() {
        let browser = browser_of(&id);
        let (mut e, same, xo) = served_page(&id).await;
        let got = run_section(&mut e, &same, &xo, "location").await;
        assert_lines(&format!("{id} location"), &got, &fixture(&browser, "location"));
    }
}

/// A SAME-DOCUMENT navigation through `location.hash`/`href`/`assign`/`replace`, and the session
/// history's `pushState`/`replaceState`/`back`/`forward`/`go`: what moves synchronously, which events
/// fire (trusted, non-bubbling, with `state` / `oldURL` / `newURL`), in which order against a microtask
/// and a `setTimeout(0)`, and how `history.length` moves -- line for line against each bundle's own
/// browser. Earlier every `location.hash = x` did nothing and `back()` was a no-op.
#[tokio::test]
async fn a_same_document_navigation_fires_like_the_browsers() {
    for id in bundle_ids() {
        let browser = browser_of(&id);
        let (mut e, same, xo) = served_page(&id).await;
        let got = run_section(&mut e, &same, &xo, "fragment").await;
        assert_lines(&format!("{id} fragment"), &got, &fixture(&browser, "fragment"));
    }
}

/// Every `Location` setter, `window.location =`, `document.location =` and the three methods: where the
/// page is headed and whether it is a new document, against `fixtures/p11a/<browser>.nav.txt`.
///
/// A cross-document navigation here is the `NavRequest` the driver loads -- that is where this engine's
/// navigation ends -- so its target URL is compared with the URL the browser ended up on, and the
/// synchronous half (the setter's return value, `location.href` before the navigation commits) line for
/// line. `history.length` is compared for the same-document case only: a new document's entry is the
/// driver's to add.
#[tokio::test]
async fn every_location_setter_navigates_where_the_browsers_do() {
    for id in bundle_ids() {
        let browser = browser_of(&id);
        let want = fixture(&browser, "nav");
        let mut compared = 0;
        for line in want.lines() {
            let (label, rest) = line.split_once(" => ").unwrap();
            let (sync_part, after_part) = rest.split_once(" || after ").unwrap();
            let (want_url, tail) = after_part.split_once(" dh=").unwrap();
            let (want_dh, want_reloaded) = tail.split_once(" reloaded=").unwrap();
            let (mut e, same, xo) = served_page(&id).await;
            let same_port = same.trim_end_matches('/').rsplit(':').next().unwrap().to_string();
            let xo_port = xo.trim_end_matches('/').rsplit(':').next().unwrap().to_string();
            let expr = match label {
                "location.host = \"localhost:PORT\"" => format!("location.host = \"localhost:{same_port}\""),
                "location.port = \"OTHER\"" => format!("location.port = \"{xo_port}\""),
                "self = 1 then location" => "self = 1, typeof self".to_string(),
                other => other.to_string(),
            };
            let h0 = e.eval_value("p11a-nav-h0", "window.__mark = 1; history.length".into()).unwrap().as_f64().unwrap();
            let probe = format!(
                "(() => {{ let r; try {{ r = String({expr}); }} catch (e) {{ r = 'THROW ' + e.name + ': ' + e.message; }} return r + ' | sync href=' + location.href.replace({same:?}, '<same>/') + ' mark=' + window.__mark; }})()"
            );
            let sync = e.eval_value("p11a-nav", probe).unwrap();
            let sync = sync.as_str().unwrap().replace(&same_port, "<same-port>").replace(&xo_port, "<xo-port>");
            let _ = tokio::time::timeout(std::time::Duration::from_millis(300), e.run()).await;
            let nav = e.take_nav_request();
            let norm = |u: &str| {
                u.replace(&same, "<same>/")
                    .replace(&format!("http://localhost:{same_port}/"), "<same-localhost>/")
                    .replace(&xo, "<xo>/")
            };
            assert_eq!(sync, sync_part, "{id} {label}: the synchronous half");
            match nav {
                Some(n) => {
                    let u = n.url;
                    assert_eq!(want_reloaded, "true", "{id} {label}: korvus loads a new document ({u}) where {browser} does not");
                    assert_eq!(norm(&u), want_url, "{id} {label}: where the new document is loaded from");
                }
                None => {
                    assert_eq!(want_reloaded, "false", "{id} {label}: {browser} loads a new document ({want_url}) and korvus does not");
                    let href = e.eval_value("p11a-nav-after", "location.href + ' ' + history.length".into()).unwrap();
                    let (href, len) = href.as_str().unwrap().split_once(' ').map(|(a, b)| (a.to_string(), b.to_string())).unwrap();
                    assert_eq!(norm(&href), want_url, "{id} {label}: the URL after a same-document navigation");
                    let dh = len.parse::<f64>().unwrap() - h0;
                    assert_eq!(format!("{dh}"), want_dh, "{id} {label}: history.length moved by");
                }
            }
            compared += 1;
        }
        assert!(compared >= 17, "{id}: {compared} navigation cases");
    }
}


/// `document.startViewTransition`: the object, `types`, `transitionRoot`, and the ORDER the callback and
/// the three promises settle in against a microtask -- for no callback, a sync, an async and a throwing
/// callback, an immediate `skipTransition()`, and the options dictionary. Each bundle against its own
/// browser's TWO recordings: a line the two disagree on is the oracle's noise (M2) and is not compared
/// (Opera 135's async-callback `ready`, which fulfilled in one of six recordings and rejected in five).
/// Earlier it answered `null`.
#[tokio::test]
async fn document_start_view_transition_settles_like_the_browsers() {
    for id in bundle_ids() {
        let browser = browser_of(&id);
        let (mut e, same, xo) = served_page(&id).await;
        let got = run_section(&mut e, &same, &xo, "vt").await;
        let (run1, run2) = (fixture(&browser, "vt"), fixture(&browser, "vt.run2"));
        let (r1, r2): (Vec<&str>, Vec<&str>) = (run1.lines().collect(), run2.lines().collect());
        assert_eq!(r1.len(), r2.len(), "{id}: the two recordings have different shapes");
        let g: Vec<&str> = got.lines().collect();
        assert_eq!(g.len(), r1.len(), "{id}: korvus printed {} lines, {browser} {}:\n{got}", g.len(), r1.len());
        let mut compared = 0;
        for i in 0..r1.len() {
            if r1[i] != r2[i] {
                eprintln!("{id}: not compared, {browser} disagrees with itself: {} / {}", r1[i], r2[i]);
                continue;
            }
            assert_eq!(g[i], r1[i], "{id}: line {i}");
            compared += 1;
        }
        assert!(compared >= r1.len() - 1, "{id}: only {compared} of {} lines compared", r1.len());
    }
}


/// A CROSS-ORIGIN `<iframe>`'s `contentWindow`: exactly the members HTML lets cross (`window self
/// location closed frames length top opener parent blur close focus postMessage`, and `then` and three
/// well-known symbols as `undefined`), Chrome's SecurityError sentence for every other read, write,
/// define, delete, `in`, `setPrototypeOf` and `preventExtensions`, the property keys and descriptors,
/// a `null` prototype, and a `Location` that lets only `href` (set) and `replace` through -- line for
/// line against each bundle's own browser. Earlier every name read straight through.
#[tokio::test]
async fn a_cross_origin_frame_exposes_only_what_the_browsers_do() {
    for id in bundle_ids() {
        let browser = browser_of(&id);
        let (mut e, same, xo) = served_page(&id).await;
        let got = run_section(&mut e, &same, &xo, "xo").await;
        assert_lines(&format!("{id} xo"), &got, &fixture(&browser, "xo"));
    }
}

// ── the live oracle comparison ───────────────────────────────────────────────────────────────────

const PROBE: &str = include_str!("fixtures/p11a/probe.js");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/p11a");
const DOC: &[u8] = b"<!doctype html><html><head></head><body></body></html>";

/// One HTTP/1.1 exchange -- the routes `fixtures/p11a/record.py` serves the browsers.
fn serve_one(mut s: std::net::TcpStream) {
    use std::io::{Read, Write};
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf).to_string();
    let path = head.split_whitespace().nth(1).unwrap_or("/").split(['?', '#']).next().unwrap_or("/").to_string();
    let ok = matches!(path.as_str(), "/page.html" | "/child.html" | "/p2.html" | "/x");
    let body: &[u8] = if ok { DOC } else { b"not found" };
    let h = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        if ok { "200 OK" } else { "404 Not Found" },
        if ok { "text/html" } else { "text/plain" },
        body.len()
    );
    let _ = s.write_all(h.as_bytes());
    let _ = s.write_all(body);
    let _ = s.flush();
}

/// A server on its OWN OS thread, so nothing it does waits on the isolate's thread.
fn start_server() -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            std::thread::spawn(move || serve_one(s));
        }
    });
    addr
}

async fn pump_for(e: &mut Engine, tag: &str) -> String {
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(i) = out.iter().position(|l| l.starts_with(tag)) {
            return out[i][tag.len()..].to_string();
        }
        if start.elapsed() > std::time::Duration::from_secs(120) {
            panic!("{tag} never arrived within 120s; output: {out:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// The engine wearing bundle `id`, on `page.html` of one loopback origin, with a second origin beside
/// it -- the page every browser fixture was recorded on.
async fn served_page(id: &str) -> (Engine, String, String) {
    let same = start_server();
    let xo = start_server();
    let mut e = Engine::new(persona::worn_on_host(persona::CapturedPersona::select(Some(id)).unwrap()));
    e.run().await.expect("engine boot");
    let url = format!("http://{same}/page.html");
    e.set_url(&url);
    e.load_page(std::str::from_utf8(DOC).unwrap(), &url, &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(std::time::Duration::from_secs(5)).await;
    (e, format!("http://{same}/"), format!("http://{xo}/"))
}

async fn run_section(e: &mut Engine, same: &str, xo: &str, section: &str) -> String {
    let tag = format!("P11A_{section}:");
    e.eval(
        "p11a_probe",
        format!(
            "({PROBE})({}, {}, {}).then((s) => print({tag:?} + JSON.stringify(s)), (err) => print({tag:?} + JSON.stringify('PROBE FAILED ' + err + ' ' + (err && err.stack))));",
            serde_json::to_string(same).unwrap(),
            serde_json::to_string(xo).unwrap(),
            serde_json::to_string(section).unwrap(),
        ),
    )
    .unwrap();
    let raw = pump_for(e, &tag).await;
    let s: String = serde_json::from_str(&raw).expect("the probe's result is a JSON string");
    // The recorder's origins had other ports; a port is not a fact about the engine.
    s.replace(same.trim_end_matches('/'), "<same-origin>").replace(xo.trim_end_matches('/'), "<xo-origin>")
}

/// The browser a bundle was captured from, as the fixture prefix: `chrome-152-linux-…` and
/// `chrome-152-windows-…` both -> `chrome-152`. The window surface these fixtures pin is the engine's
/// JS-level behaviour (accessors, descriptors, location, navigation), which is the browser's whatever
/// OS it runs on, so a Windows persona grades against the same family+version fixture a Linux one does.
fn browser_of(id: &str) -> String {
    id.split('-').take(2).collect::<Vec<_>>().join("-")
}

fn fixture(browser: &str, section: &str) -> String {
    let p = std::path::Path::new(FIXTURES).join(format!("{browser}.{section}.txt"));
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display())).trim_end().to_string()
}

fn keyed(text: &str) -> std::collections::BTreeMap<String, String> {
    text.lines().filter_map(|l| l.split_once(" => ")).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// Line-for-line equality with the browser's recording.
fn assert_lines(label: &str, got: &str, want: &str) {
    let g: Vec<&str> = got.lines().collect();
    let w: Vec<&str> = want.lines().collect();
    let mut diffs = Vec::new();
    for i in 0..g.len().max(w.len()) {
        let (a, b) = (w.get(i).copied().unwrap_or("<missing>"), g.get(i).copied().unwrap_or("<missing>"));
        if a != b {
            diffs.push(format!("  browser: {a}\n  korvus:  {b}"));
        }
    }
    assert!(diffs.is_empty(), "[{label}] {} of {} lines differ:\n{}", diffs.len(), w.len(), diffs.join("\n"));
}

/// Keyed equality on every key both sides have, and the keys only one side has returned: a descriptor
/// SHAPE is compared for every name the engine installs; which NAMES exist is the persona surface's
/// question (`persona_surface::` and its `MISSING_GLOBALS_CEILING`), not this file's.
fn assert_shared_keys(label: &str, got: &str, want: &str, scope: impl Fn(&str) -> bool) -> (Vec<String>, Vec<String>) {
    let (g, w) = (keyed(got), keyed(want));
    let mut diffs = Vec::new();
    for (k, wv) in w.iter().filter(|(k, _)| scope(k)) {
        if let Some(gv) = g.get(k) {
            if gv != wv {
                diffs.push(format!("  {k}\n    browser: {wv}\n    korvus:  {gv}"));
            }
        }
    }
    assert!(diffs.is_empty(), "[{label}] {} shared keys differ:\n{}", diffs.len(), diffs.join("\n"));
    let only_browser = w.keys().filter(|k| scope(k) && !g.contains_key(*k)).cloned().collect();
    let only_korvus = g.keys().filter(|k| scope(k) && !w.contains_key(*k)).cloned().collect();
    (only_browser, only_korvus)
}

/// A key names a WINDOW OWN PROPERTY (`window.X`, or `window.X = 7`) -- the one place a key may be
/// one-sided, because which globals exist is the persona's.
fn is_window_global_key(k: &str) -> bool {
    k.starts_with("window.")
}
