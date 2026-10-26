//! `drawImage` draws, a cross-origin image taints, WebGL extensions exist and a context
//! can be lost, and `crypto`/`indexedDB` are accessors. Live, against two loopback origins.
//!
//! # The defect, measured before a line changed
//!
//! Release binary built at `8f5aee08` against stock Chrome 150.0.7871.46, the same page on both:
//!
//! ```text
//! <img src=red.png 20x20> onload, drawImage(img,0,0), getImageData(5,5,1,1)   korvus 0,0,0,0   chrome 255,0,0,255
//! cross-origin <img>, drawImage, toDataURL()                                   korvus no throw  chrome SecurityError
//! typeof gl.getExtension("WEBGL_lose_context")?.loseContext                    korvus undefined chrome function
//! Object.getOwnPropertyDescriptor(window, "crypto")                            korvus data      chrome accessor
//! Object.getOwnPropertyDescriptor(window, "indexedDB")                         korvus data      chrome accessor
//! ```
//!
//! # The oracle
//!
//! `fixtures/p10d/canvas-probe.js` is evaluated UNMODIFIED here and in Chrome, section by section,
//! and every line Chrome printed is asserted. `fixtures/p10d/*.chrome-150*.txt` are Chrome's output,
//! RECORDED -- each twice, identical -- against `fixtures/p10d/canvas-probe-server.py`, which serves
//! the same routes this file's server does. `gpu-ext` is recorded from a Chrome started with
//! `--use-angle=gl-egl`, because the six extensions it prints exist only on a GPU backend; the
//! persona bundles all list them, so the engine must answer their shapes.
//!
//! Re-record: `python3 canvas-probe-server.py <a> <b>`, open `http://127.0.0.1:<a>/page.html` in a
//! headless Chrome over CDP, evaluate `(<canvas-probe.js>)("http://127.0.0.1:<a>/",
//! "http://127.0.0.1:<b>/", "<section>")` with `awaitPromise`, and write the string plus "\n". A line
//! that differs here is a finding about the engine, never a reason to edit a `.txt`.
//!
//! `webgl.chrome-150.txt` was re-recorded 2026-10-12 on the headed desktop Chrome with no DevTools,
//! three times alike: which extensions exist is the GPU's, and this machine's Chrome now draws on its RTX 4060
//! (`WEBGL_draw_buffers` and `OVR_multiview2` are `null` there, as they are in korvus on the same GPU).
//!
//! Two Chrome answers depend on the DEVICE and not on the engine, and the probe asks around them
//! rather than recording another machine's values: `getParameter(MAX_DRAW_BUFFERS)` prints its type,
//! and no unmasked renderer string is printed at all.
//!
//! # What is NOT asserted, because the engine does not answer it yet (measured, P10d report)
//!
//! Smoothed resampling INSIDE a scaled image. Pixels that sample one colour, and the smoothed edge of
//! a 2x one-quadrant draw, are Chrome's exactly (asserted). Three probes are not:
//!
//! ```text
//! drawImage(quad, 0,0,40,40) pixel 19,19 (smoothing low)       chrome 156,57,49,247  korvus 156,58,50,247
//! the same with imageSmoothingQuality = "high"                 chrome 156,57,49,247  korvus 156,58,50,247
//! drawImage(quad, 0,0,100,100, 0,0,20,20) pixel 2,2 (5x down)  chrome 140,185,57,170 korvus 255,255,0,128
//! ```
//!
//! The first two are one unit of rounding in a premultiplied bilinear blend (before this unit, which
//! sampled the image unpremultiplied, they read 159,64,47,247); the third is Chrome's downscale filter,
//! which samples a footprint this backend's `Linear`/no-mipmap `low` quality does not.

use engine::Engine;
use std::io::{Read, Write};

const PROBE: &str = include_str!("fixtures/p10d/canvas-probe.js");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/p10d");

fn content_type(name: &str) -> Option<&'static str> {
    match name.rsplit('.').next()? {
        "png" => Some("image/png"),
        "jpg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "bmp" => Some("image/bmp"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// One HTTP/1.1 exchange -- the routes `canvas-probe-server.py` serves.
fn serve_one(mut s: std::net::TcpStream) {
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
    let path = head.split_whitespace().nth(1).unwrap_or("/").split('?').next().unwrap_or("/").to_string();
    let respond = |s: &mut std::net::TcpStream, status: &str, ctype: &str, acao: bool, body: &[u8]| {
        let mut h = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n", body.len());
        if acao {
            h.push_str("Access-Control-Allow-Origin: *\r\n");
        }
        h.push_str("\r\n");
        let _ = s.write_all(h.as_bytes());
        let _ = s.write_all(body);
        let _ = s.flush();
    };
    if path == "/page.html" {
        return respond(&mut s, "200 OK", "text/html", false, b"<!doctype html><html><head></head><body></body></html>");
    }
    let name = path.rsplit('/').next().unwrap_or("");
    let file = std::path::Path::new(FIXTURES).join(name);
    match (content_type(name), std::fs::read(&file)) {
        (Some(ct), Ok(bytes)) if !name.contains("..") => {
            let body: Vec<u8> = if path.starts_with("/garbage/") { b"this is not an image at all".to_vec() } else { bytes };
            respond(&mut s, "200 OK", ct, path.starts_with("/cors/"), &body)
        }
        _ => respond(&mut s, "404 Not Found", "text/plain", false, b"not found"),
    }
}

/// A server on its OWN OS thread, so nothing it does waits on the isolate's thread.
fn start_server() -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            match conn {
                Ok(s) => {
                    std::thread::spawn(move || serve_one(s));
                }
                Err(_) => break,
            }
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

/// The engine, booted on `page.html` of one loopback origin, with a second origin beside it.
async fn page(persona: persona::CapturedPersona) -> (Engine, String, String) {
    let same = start_server();
    let xo = start_server();
    let mut e = Engine::new(persona::worn_on_host(persona));
    e.run().await.expect("engine boot");
    let doc = "<!doctype html><html><head></head><body></body></html>";
    e.load_page(doc, &format!("http://{same}/page.html"), &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(std::time::Duration::from_secs(5)).await;
    (e, format!("http://{same}/"), format!("http://{xo}/"))
}

async fn run_section(e: &mut Engine, same: &str, xo: &str, section: &str) -> String {
    let tag = format!("P10D_{section}:");
    e.eval(
        "p10d_probe",
        format!(
            "({PROBE})({}, {}, {}).then((s) => print({tag:?} + JSON.stringify(s)), (err) => print({tag:?} + JSON.stringify('PROBE FAILED ' + err + ' ' + (err && err.stack))));",
            serde_json::to_string(same).unwrap(),
            serde_json::to_string(xo).unwrap(),
            serde_json::to_string(section).unwrap(),
        ),
    )
    .unwrap();
    let raw = pump_for(e, &tag).await;
    serde_json::from_str(&raw).expect("the probe's result is a JSON string")
}

fn assert_like_chrome(section: &str, got: &str, chrome: &str) {
    let got_lines: Vec<&str> = got.lines().collect();
    let want_lines: Vec<&str> = chrome.lines().collect();
    let mut diffs = Vec::new();
    for i in 0..want_lines.len().max(got_lines.len()) {
        let w = want_lines.get(i).copied().unwrap_or("<missing>");
        let g = got_lines.get(i).copied().unwrap_or("<missing>");
        if w != g {
            diffs.push(format!("  chrome: {w}\n  korvus: {g}"));
        }
    }
    assert!(diffs.is_empty(), "[{section}] {} of {} lines differ from Chrome 150:\n{}", diffs.len(), want_lines.len(), diffs.join("\n"));
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(std::path::Path::new(FIXTURES).join(name)).unwrap_or_else(|e| panic!("read {name}: {e}"))
}

#[tokio::test]
async fn draw_image_decodes_and_a_cross_origin_image_taints_like_chrome() {
    let (mut e, same, xo) = page(persona::CapturedPersona::default()).await;
    let got = run_section(&mut e, &same, &xo, "canvas").await;
    assert_like_chrome("canvas", &got, &fixture("canvas.chrome-150.txt"));
}

#[tokio::test]
async fn crypto_and_indexeddb_are_accessors_and_create_image_bitmap_exists_like_chrome() {
    let (mut e, same, xo) = page(persona::CapturedPersona::default()).await;
    let got = run_section(&mut e, &same, &xo, "globals").await;
    assert_like_chrome("globals", &got, &fixture("globals.chrome-150.txt"));
}

#[tokio::test]
async fn webgl_extensions_exist_and_a_context_can_be_lost_like_chrome() {
    let (mut e, same, xo) = page(persona::CapturedPersona::default()).await;
    let got = run_section(&mut e, &same, &xo, "webgl").await;
    assert_like_chrome("webgl", &got, &fixture("webgl.chrome-150.txt"));
    let got = run_section(&mut e, &same, &xo, "gpu-ext").await;
    assert_like_chrome("gpu-ext", &got, &fixture("gpu-ext.chrome-150-gl-egl.txt"));
}

/// THE DEVICE DECIDES WHICH EXTENSIONS EXIST, under every bundle in the tree: `getSupportedExtensions()`
/// is the list this PC's GPU offers through Chrome's ANGLE (the bundle's recorded list on a
/// backend with no device), every name on it hands back a real extension object (never `null`, never a
/// bare `{}`), and a name this engine has a table for but the list does NOT have is `null`. A loop over
/// the bundle directory, so a fifth bundle is under gate without an edit.
#[tokio::test]
async fn every_bundle_offers_exactly_its_own_extension_list() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/korvus-persona/bundles");
    let mut ids: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|d| d.ok())
        .filter(|d| d.path().join("manifest.toml").is_file())
        .map(|d| d.file_name().to_string_lossy().to_string())
        .collect();
    ids.sort();
    assert!(!ids.is_empty(), "no persona bundle in the tree");
    for id in ids {
        let values: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(&id).join("values.json")).unwrap()).unwrap();
        let persona = persona::CapturedPersona::select(Some(&id)).unwrap();
        let (mut e, _, _) = page(persona).await;
        let webgl = crate::webgl_device::page_webgl(&values["deviceOracle"].to_string());
        for (ctx, key) in [("webgl", "extensions"), ("webgl2", "extensionsV2")] {
            let listed: Vec<String> = webgl[key]
                .as_array()
                .unwrap_or_else(|| panic!("{id}: no {key}"))
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
            let listed_json = serde_json::to_string(&listed).unwrap();
            let js = format!(
                r#"(() => {{
                    const g = document.createElement('canvas').getContext({ctx:?});
                    const listed = {listed_json};
                    const all = ['WEBGL_lose_context', 'ANGLE_instanced_arrays', 'OES_vertex_array_object', 'WEBGL_draw_buffers', 'EXT_color_buffer_float', 'EXT_disjoint_timer_query', 'EXT_disjoint_timer_query_webgl2', 'WEBGL_compressed_texture_astc', 'WEBGL_compressed_texture_etc', 'WEBGL_provoking_vertex', 'OES_texture_float'];
                    const bad = [];
                    if (JSON.stringify(g.getSupportedExtensions()) !== JSON.stringify(listed)) bad.push('list ' + JSON.stringify(g.getSupportedExtensions()));
                    for (const n of listed) {{
                        const e = g.getExtension(n);
                        const tag = Object.prototype.toString.call(e);
                        if (!e || tag === '[object Object]' || e !== g.getExtension(n)) bad.push(n + ' -> ' + tag);
                    }}
                    for (const n of all) if (!listed.includes(n) && g.getExtension(n) !== null) bad.push(n + ' is not listed and answered');
                    return bad.join(' | ') || 'ok';
                }})()"#
            );
            e.eval("p10d_exts", format!("print('P10D_EXTS:' + {js})")).unwrap();
            let got = pump_for(&mut e, "P10D_EXTS:").await;
            assert_eq!(got, "ok", "bundle {id}, {ctx}");
        }
    }
}

/// A WebP image draws its pixels, as Chrome's does: this build's Skia has no WebP decoder (the prebuilt's
/// `webp-decode` feature is off), and `plugins/korvus-render`'s `modern_decode` decodes WebP and AVIF instead. Until
/// it did, such an image drew nothing -- and was still NOT BROKEN: it threw none of the InvalidStateError Chrome
/// never throws for it. The fixture's top-left quadrant is red.
#[tokio::test]
async fn a_webp_image_draws_its_pixels_and_does_not_throw() {
    let (mut e, same, _) = page(persona::CapturedPersona::default()).await;
    let js = format!(
        r#"new Promise((r) => {{ const i = new Image(); i.onload = i.onerror = () => r(i); i.src = {src:?}; }}).then((i) => {{
            const c = document.createElement('canvas'); c.width = 20; c.height = 20; const x = c.getContext('2d');
            try {{ x.drawImage(i, 0, 0); return 'ok ' + Array.from(x.getImageData(2, 2, 1, 1).data).join(','); }} catch (e) {{ return 'THROW ' + e.name; }}
        }}).then((s) => print('P10D_WEBP:' + s))"#,
        src = format!("{same}quad.webp")
    );
    e.eval("p10d_webp", js).unwrap();
    assert_eq!(pump_for(&mut e, "P10D_WEBP:").await, "ok 255,0,0,255");
}

/// A `data:` IMAGE NEVER TAINTS. Fetch's "main fetch" gives a `data:` request
/// "basic" response tainting, so the image is CORS-same-origin with or without `crossorigin` and
/// `getImageData` reads it back. The engine compared the URL's OPAQUE origin with the document's
/// and called it cross-origin -- unreachable until `data:` images loaded, and then
/// learn.microsoft.com's APNG test (a 1x1 `data:image/png`, `crossOrigin = "anonymous"`, drawn and read
/// back) threw "The canvas has been tainted by cross-origin data" where Chrome's replay reference
/// throws nothing.
#[tokio::test]
async fn a_data_url_image_is_same_origin_and_never_taints() {
    let (mut e, _, _) = page(persona::CapturedPersona::default()).await;
    let js = r#"(function () {
        const src = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==';
        const one = (co) => new Promise((r) => { const i = new Image(); if (co) i.crossOrigin = co; i.onload = i.onerror = () => r(i); i.src = src; }).then((i) => {
            const c = document.createElement('canvas'); c.width = 1; c.height = 1; const x = c.getContext('2d');
            try { x.drawImage(i, 0, 0); return Array.from(x.getImageData(0, 0, 1, 1).data).join(',') + ' ' + c.toDataURL().slice(0, 22); }
            catch (e) { return 'THROW ' + e.name; }
        });
        return Promise.all([one(null), one('anonymous')]);
    })().then((s) => print('P10D_DATA:' + JSON.stringify(s)))"#;
    e.eval("p10d_data", js.to_string()).unwrap();
    assert_eq!(
        pump_for(&mut e, "P10D_DATA:").await,
        r#"["255,0,0,255 data:image/png;base64,","255,0,0,255 data:image/png;base64,"]"#
    );
}
