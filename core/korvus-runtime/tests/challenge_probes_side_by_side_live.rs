//! LIVE gate: what the side-by-side trace of a live an interactive-widget challenge challenge found -- the
//! challenge frame's reads on which this engine answered differently from the traced Chrome 150 beside it -- each
//! answering as stock Chrome 150 does.
//!
//! * `canvas-colors*`, `css-colors`, `system-colors`, `color-readback`: CSS Color 4/5 as Blink holds it
//!   (`page::css_color`). The challenge fills four canvases with `color(display-p3 1 0.25 0.5)` and reads them back
//!   (sRGB / display-p3, unorm8 / float16); the engine parsed no `color()`, kept the previous fill and read black.
//!   The colour is kept as written (`fillStyle`, `el.style.color`, `getComputedStyle`), painted with Blink's
//!   `toSkColor4f` float for float, taken into the canvas's colour space by Skia, read back through Skia's
//!   `readPixels` into the requested space and format; gradients interpolate as Blink asks Skia to (OKLab once a stop
//!   is not legacy, else sRGB, with Blink's own end padding); `color-mix()`, relative colours, `calc()` channels,
//!   `contrast-color()`, the system colours (a `<canvas>`'s and an `OffscreenCanvas`'s apart), `currentColor` (a
//!   connected canvas's inline `color`), non-string `fillStyle` values (ToString).
//! * `imagedata-formats*`: `ImageData` in `rgba-unorm8` / `rgba-float16` / `rgba-float32`, `ImageDataSettings` read
//!   in Blink's two phases, `getContextAttributes()` in WebIDL's member order.
//! * `style-declaration*`: `CSSStyleDeclaration`'s named and indexed properties as Blink's interceptors answer them.
//! * `rtp-capabilities`: `RTCRtpReceiver/Sender.getCapabilities` from the persona bundle (the receiver's VIDEO list
//!   is left out: it is the device's hardware decoders, which the bundle records and this machine's Chrome adds to).
//! * `canvas-contexts`, `context-ids`, `context-attributes*`: the two `getContext`s. An `OffscreenCanvas` 2D context
//!   has no own `canvas` and keeps its canvas's back-reference (`getImageData` beyond 300x150 read nothing), an
//!   `OffscreenCanvas`'s `direction` is the snapshot Blink takes (`<html dir>`, or the element's on transfer), a
//!   detached canvas's is its `dir`; the id is matched exactly (an `OffscreenRenderingContextType` enum TypeError on an
//!   `OffscreenCanvas`); `CanvasContextCreationAttributesModule` is read whole, in WebIDL's order, on every call, its
//!   enums refused with Blink's sentences. The circle the `OffscreenCanvas` probe drew is left out: four edge pixels
//!   of an anti-aliased oval differ by 1-4/255 in its lower-right quadrant, from Skia sources one Mac-only commit
//!   apart and the same path, matrix and blitter: not the conic chop (`SK_SUPPORT_LEGACY_CONIC_CHOP` yields the same
//!   quads), the clip, the start index or the blitter -- a residual below the engine, open until Skia is built as
//!   Chrome builds it.
//! * `perf-timeline`: paint timing as Blink keeps it (`startTime` the frame's presentation, coarsened up to 4 ms,
//!   `paintTime` the update's end, the entry on the timeline at the presentation; `toJSON` with the pair) and long
//!   animation frames (`long-animation-frame` in `getEntries()`, a long task alone and one that rendered).
//! * `engine-url-reads`: the engine's own URL work never constructs the page's `URL` (a cross-origin frame's
//!   messages and location, a frame's navigation).
//! * `engine-promise-reads`: the engine's own promise chaining and header lists (fetch, XHR, `responseXML`, a
//!   worker's fetch) call no `then`, iterator or `parseFromString` a page replaced.
//! * `perf-observer`: a PerformanceObserver's callback arguments and `this`, `observe()`'s dictionary and its
//!   errors, `takeRecords()` (found by korvus-analysis's `ka` against a stock trace, 2026-10-09).
//! * served, on two loopback origins: a cross-origin frame's `Notification.permission` and permission state, a
//!   worker's message built in the receiving realm, the challenge image (a PNG whose `IDAT` is raw deflate: `load`,
//!   65x51, nothing drawn), paint timing per frame, and PNG decode semantics (`decode()`, a truncated stream drawn to
//!   its last row, `createImageBitmap` of an incomplete or broken image).
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-08**, each probe served from
//! 127.0.0.1 (and its frames from `localhost`). The canvas PIXELS (`canvas-colors`, `color-readback`) are software
//! Chrome's (`--disable-gpu`): an accelerated canvas hands the GPU a wide paint colour as halves its CPU code
//! truncates, and the GPU's blending rounds otherwise, so translucent and float16 pixels are the device's -- this
//! engine rasterizes as Chrome's CPU raster does (opaque colours read the same both ways, the challenge's
//! `[255, 27, 127]` and `[255, 64, 127]` among them).

use crate::keyboard_live::{answers, CHROME_BUNDLE};
use engine::Engine;
use std::io::{Read, Write};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/challenge_probes");

async fn check(script: &str, recorded: &str) {
    let got = answers(CHROME_BUNDLE, script).await;
    assert_rows_like_chrome(&serde_json::Value::Array(got), recorded);
}

/// A probe's answer list against Chrome's, row by row (a page that did not answer a list is shown as it answered).
fn assert_rows_like_chrome(got: &serde_json::Value, recorded: &str) {
    let want: Vec<serde_json::Value> = serde_json::from_str(recorded).expect("the recorded Chrome list");
    let Some(got) = got.as_array() else { panic!("the page answered {got}, not a list") };
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    let differ: Vec<String> = got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect();
    assert!(differ.is_empty(), "{} answers differ from Chrome 150:\n{}", differ.len(), differ.join("\n"));
}

#[tokio::test]
async fn canvas_colors_paint_and_read_back_as_chrome_150_does() {
    check(include_str!("fixtures/challenge_probes/canvas-colors-probe.js"), include_str!("fixtures/challenge_probes/canvas-colors-chrome-150.json")).await;
}

#[tokio::test]
async fn canvas_system_colors_current_color_and_style_values_answer_as_chrome_150_does() {
    check(include_str!("fixtures/challenge_probes/canvas-colors-element-probe.js"), include_str!("fixtures/challenge_probes/canvas-colors-element-chrome-150.json")).await;
}

#[tokio::test]
async fn css_colors_parse_serialize_and_compute_as_chrome_150_does() {
    check(include_str!("fixtures/challenge_probes/css-colors-probe.js"), include_str!("fixtures/challenge_probes/css-colors-chrome-152.json")).await;
}

#[tokio::test]
async fn system_colors_resolve_as_chrome_150_does() {
    check(include_str!("fixtures/challenge_probes/system-colors-probe.js"), include_str!("fixtures/challenge_probes/system-colors-chrome-150.json")).await;
}

#[tokio::test]
async fn a_display_p3_fill_reads_back_in_every_space_and_format_as_chrome_150_does() {
    check(include_str!("fixtures/challenge_probes/color-readback-probe.js"), include_str!("fixtures/challenge_probes/color-readback-chrome-150.json")).await;
}

#[tokio::test]
async fn image_data_formats_answer_as_chrome_150_does() {
    check(include_str!("fixtures/challenge_probes/imagedata-formats-probe.js"), include_str!("fixtures/challenge_probes/imagedata-formats-chrome-152.json")).await;
    check(include_str!("fixtures/challenge_probes/imagedata-formats-2-probe.js"), include_str!("fixtures/challenge_probes/imagedata-formats-2-chrome-152.json")).await;
    check(include_str!("fixtures/challenge_probes/imagedata-formats-3-probe.js"), include_str!("fixtures/challenge_probes/imagedata-formats-3-chrome-152.json")).await;
    check(include_str!("fixtures/challenge_probes/imagedata-formats-4-probe.js"), include_str!("fixtures/challenge_probes/imagedata-formats-4-chrome-152.json")).await;
}

#[tokio::test]
async fn style_declarations_answer_named_and_indexed_reads_as_chrome_150_does() {
    check(include_str!("fixtures/challenge_probes/style-declaration-probe.js"), include_str!("fixtures/challenge_probes/style-declaration-chrome-152.json")).await;
    check(include_str!("fixtures/challenge_probes/style-declaration-2-probe.js"), include_str!("fixtures/challenge_probes/style-declaration-2-chrome-152.json")).await;
}

#[tokio::test]
async fn canvas_contexts_keep_their_canvas_and_direction_as_chrome_150_does() {
    check(include_str!("fixtures/challenge_probes/canvas-contexts-probe.js"), include_str!("fixtures/challenge_probes/canvas-contexts-chrome-152.json")).await;
}

#[tokio::test]
async fn get_context_matches_its_id_and_reads_its_attributes_as_chrome_150_does() {
    check(include_str!("fixtures/challenge_probes/context-ids-probe.js"), include_str!("fixtures/challenge_probes/context-ids-chrome-152.json")).await;
    check(include_str!("fixtures/challenge_probes/context-attributes-probe.js"), include_str!("fixtures/challenge_probes/context-attributes-chrome-152.json")).await;
    check(include_str!("fixtures/challenge_probes/context-attributes-reads-probe.js"), include_str!("fixtures/challenge_probes/context-attributes-reads-chrome-152.json")).await;
}

/// Served and pumped: paint timing and long animation frames come from the event loop's rendering steps, which only
/// `pump` runs.
#[tokio::test]
async fn paint_timing_and_long_animation_frames_answer_as_chrome_150_does() {
    let probe = include_str!("fixtures/challenge_probes/perf-timeline-probe.js");
    let doc = format!("<!doctype html><html><head><meta charset=utf-8></head><body><script>window.__done = {probe};</script></body></html>");
    assert_rows_like_chrome(&run_page(&doc).await, include_str!("fixtures/challenge_probes/perf-timeline-chrome-152.json"));
}

/// PerformanceObserver as Blink runs it: the callback invoked with the observer as `this` and
/// `{droppedEntriesCount}` third after each `observe()`; `observe()`'s dictionary read once in code-point
/// order with `entryTypes` a WebIDL sequence; the observer-type lock both ways (an `InvalidModificationError`
/// in Blink's words, kept across `disconnect`); `takeRecords()` handing back what is queued.
#[tokio::test]
async fn performance_observers_answer_as_chrome_150_does() {
    check(include_str!("fixtures/challenge_probes/perf-observer-probe.js"), include_str!("fixtures/challenge_probes/perf-observer-chrome-152.json")).await;
}

#[tokio::test]
async fn rtp_capabilities_answer_as_chrome_150_does() {
    check(include_str!("fixtures/challenge_probes/rtp-capabilities-probe.js"), include_str!("fixtures/challenge_probes/rtp-capabilities-chrome-152.json")).await;
}

// ---- served -------------------------------------------------------------------------------------------

/// The cross-origin probe's frame, on `localhost`: it runs the probe and posts its answers up.
fn frame_page(probe: &str) -> String {
    format!("<!doctype html><html><head><meta charset=utf-8></head><body><div id=d>hello</div><script>{probe}\nrun().then(r => parent.postMessage({{frame: r}}, '*'));</script></body></html>")
}

/// A paint-timing frame: `html`, and its paint entries posted up after 1.5 s.
fn paint_frame(name: &str, html: &str) -> String {
    format!("<!doctype html><body>{html}<script>setTimeout(() => parent.postMessage({{name: {name:?}, paint: performance.getEntriesByType(\"paint\").map(e => e.name).join(\",\")}}, \"*\"), 1500)</script>")
}

fn query(path: &str, key: &str) -> String {
    let q = path.split_once('?').map(|(_, q)| q).unwrap_or("");
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| {
            let bytes = v.as_bytes();
            let mut out = Vec::new();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'%' && i + 2 < bytes.len() {
                    if let Ok(b) = u8::from_str_radix(&v[i + 1..i + 3], 16) {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                }
                out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
                i += 1;
            }
            String::from_utf8_lossy(&out).to_string()
        })
        .unwrap_or_default()
}

/// One HTTP/1.1 exchange: the probe frames and the PNG fixtures.
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
    let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
    let route = path.split('?').next().unwrap_or("/");
    let (ctype, body): (&str, Vec<u8>) = match route {
        "/frame" => ("text/html; charset=utf-8", frame_page(include_str!("fixtures/challenge_probes/frame-probe.js")).into_bytes()),
        "/f" => ("text/html; charset=utf-8", paint_frame(&query(&path, "name"), &query(&path, "html")).into_bytes()),
        r if r.ends_with(".png") && !r.contains("..") => match std::fs::read(std::path::Path::new(FIXTURES).join(r.trim_start_matches('/'))) {
            Ok(b) => ("image/png", b),
            Err(_) => ("text/plain", Vec::new()),
        },
        _ => ("text/plain", Vec::new()),
    };
    let status = if body.is_empty() { "404 Not Found" } else { "200 OK" };
    let h = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", body.len());
    let _ = s.write_all(h.as_bytes());
    let _ = s.write_all(&body);
    let _ = s.flush();
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

/// The engine on `doc`, served as `http://127.0.0.1:<port>/`, until the page's `window.__done` settles: its JSON.
async fn run_page(doc: &str) -> serde_json::Value {
    let addr = start_server();
    let p = persona::CapturedPersona::select(Some(CHROME_BUNDLE)).expect("the bundle");
    let mut e = Engine::new(Box::new(p));
    e.run().await.expect("engine boot");
    let doc = doc.replace("PORT", &addr.port().to_string());
    e.load_page(&doc, &format!("http://127.0.0.1:{}/", addr.port()), &[]).await.expect("load_page");
    let tag = "SIDE_BY_SIDE:";
    e.eval("done", format!("window.__done.then(r => print({tag:?} + JSON.stringify(r)), err => print({tag:?} + JSON.stringify('FAILED ' + err)))")).unwrap();
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        // `pump`, not `run`: the event loop's rendering steps (paint timing among them) run only there.
        let _ = e.pump(std::time::Duration::from_millis(100)).await;
        out.extend(e.take_output());
        if let Some(line) = out.iter().find(|l| l.starts_with(tag)) {
            return serde_json::from_str(&line[tag.len()..]).expect("the page's answers");
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(90), "the page never finished; output: {out:?}");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

fn fixture_json(name: &str) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(std::path::Path::new(FIXTURES).join(name)).unwrap()).unwrap()
}

/// Every key Chrome recorded, compared; a difference names its path.
fn assert_like_chrome(got: &serde_json::Value, want: &serde_json::Value) {
    let mut diffs = Vec::new();
    fn walk(path: &str, g: &serde_json::Value, w: &serde_json::Value, diffs: &mut Vec<String>) {
        match (g, w) {
            (_, serde_json::Value::Object(wm)) => {
                for (k, wv) in wm {
                    walk(&format!("{path}.{k}"), g.get(k).unwrap_or(&serde_json::Value::Null), wv, diffs);
                }
            }
            _ if g != w => diffs.push(format!("{path}: korvus {g}\n    Chrome {w}")),
            _ => {}
        }
    }
    walk("", got, want, &mut diffs);
    assert!(diffs.is_empty(), "{} answers differ from Chrome 150:\n{}", diffs.len(), diffs.join("\n"));
}

/// The top page (127.0.0.1) and a cross-origin frame (`localhost`) run the same probe.
#[tokio::test]
async fn a_cross_origin_frame_answers_permission_realm_and_image_reads_as_chrome_152_does() {
    let probe = include_str!("fixtures/challenge_probes/frame-probe.js");
    let doc = format!(
        "<!doctype html><html><head><meta charset=utf-8></head><body><div id=d>hello</div><script>{probe}\n\
         window.__done = new Promise(res => window.addEventListener('message', e => {{ if (e.data && e.data.frame) run().then(t => res({{top: t, frame: e.data.frame}})); }}));\n\
         const f = document.createElement('iframe'); f.src = 'http://localhost:PORT/frame'; f.width = 300; f.height = 150; document.body.appendChild(f);</script></body></html>"
    );
    let got = run_page(&doc).await;
    assert_like_chrome(&got, &fixture_json("frame-chrome-152.json"));
}

/// The engine's own URL work -- a message's target origin, a frame's location, a frame's navigation -- never goes
/// through the page's `URL` constructor or `URL.prototype`: a page that wraps them sees nothing, as in Chrome 150.
#[tokio::test]
async fn the_engines_own_url_work_never_reaches_the_pages_url_as_in_chrome_150() {
    let probe = include_str!("fixtures/challenge_probes/engine-url-reads-probe.js");
    let doc = format!("<!doctype html><html><head><meta charset=utf-8></head><body><script>window.__done = {probe};</script></body></html>");
    assert_rows_like_chrome(&run_page(&doc).await, include_str!("fixtures/challenge_probes/engine-url-reads-chrome-150.json"));
}

/// The engine chains its own promises with V8's `then` and reads its own lists: a fetch with headers, an XHR, an
/// XHR's `responseXML` and a worker's fetch call nothing a page replaced on `Promise.prototype`, `Headers` or its
/// iterator, or `DOMParser.prototype` -- Chrome 150's one `then` is its own body read's, which this engine makes too.
#[tokio::test]
async fn the_engines_own_promises_and_lists_never_reach_the_pages_replacements_as_in_chrome_150() {
    let probe = include_str!("fixtures/challenge_probes/engine-promise-reads-probe.js");
    let doc = format!("<!doctype html><html><head><meta charset=utf-8></head><body><script>window.__done = {probe};</script></body></html>");
    assert_rows_like_chrome(&run_page(&doc).await, include_str!("fixtures/challenge_probes/engine-promise-reads-chrome-150.json"));
}

/// Paint timing per frame: only a frame that shows content paints.
#[tokio::test]
async fn only_a_frame_showing_content_paints_as_in_chrome_150() {
    let doc = r#"<!doctype html><body><div>top</div><script>
const res = {};
window.__done = new Promise(done => window.addEventListener('message', e => { res[e.data.name] = e.data.paint; if (Object.keys(res).length === 7) done(res); }));
const mk = (name, style, html) => { const f = document.createElement('iframe'); f.setAttribute('style', style); f.src = 'http://localhost:PORT/f?name=' + name + '&html=' + encodeURIComponent(html); document.body.appendChild(f); };
mk('visible', 'width:300px;height:150px', '<p>hi</p>');
mk('empty', 'width:300px;height:150px', '');
mk('none', 'display:none', '<p>hi</p>');
mk('hidden', 'visibility:hidden;width:300px;height:150px', '<p>hi</p>');
mk('zero', 'width:0;height:0;border:0', '<p>hi</p>');
mk('offscreen', 'position:absolute;left:-9999px;width:300px;height:150px', '<p>hi</p>');
mk('bodyonly', 'width:300px;height:150px', '<div style="width:10px;height:10px"></div>');
</script>"#;
    let got = run_page(doc).await;
    assert_like_chrome(&got, &fixture_json("frame-paint-chrome-150.json"));
}

/// The challenge image and three others through `<img>`, `decode()`, `drawImage`, `createPattern` and
/// `createImageBitmap`.
#[tokio::test]
async fn png_images_load_decode_and_draw_as_in_chrome_150() {
    let probe = include_str!("fixtures/challenge_probes/images-probe.js");
    let doc = format!("<!doctype html><html><body><script>window.__done = {probe};</script></body></html>");
    let got = run_page(&doc).await;
    assert_like_chrome(&got, &fixture_json("images-chrome-150.json"));
}
