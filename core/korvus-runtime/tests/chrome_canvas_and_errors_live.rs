//! Canvas pixels and error reports as stock Chrome 150 makes them. a bot challenge
//! frame draws a 48x48 scene into an OffscreenCanvas -- gradient text with a shadow under `scale(0.384)`, stroked
//! text, beziers -- and hashes `getImageData`; it runs timers, listeners and microtasks whose errors reach its
//! `onerror`. Each answer below was this engine's own until this batch.
//!
//! Every answer was recorded from `/usr/bin/google-chrome` 150.0.7871.46 on this machine, three runs alike, from the
//! fixtures in `fixtures/canvas-and-errors/` served as this file serves them: a probe page at `127.0.0.1`, or a page
//! there framing `/f` from `localhost` (another site, a renderer of its own), or a page of plain classic scripts;
//! ports read `PORT`. Pixel results are FNV-1a hashes of the `getImageData` bytes, so they depend on this machine's
//! fonts (Liberation, DejaVu, Noto CJK), as Chrome's do.

use std::io::{BufRead, BufReader, Read, Write};
use std::time::{Duration, Instant};

const PROBE_PAGE: &str = include_str!("fixtures/canvas-and-errors/probe-page.html");
const HOST_PAGE: &str = include_str!("fixtures/canvas-and-errors/host-page.html");
const FRAME_PAGE: &str = include_str!("fixtures/canvas-and-errors/frame-page.html");
const ERRORS_PAGE: &str = include_str!("fixtures/canvas-and-errors/errors-page.html");
const WRAPPING_HOST_PAGE: &str = include_str!("fixtures/canvas-and-errors/host-page-wrapping.html");

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Mode {
    /// the probe in `probe-page.html` at `/`
    Page,
    /// `host-page.html` at `/`, the probe in `frame-page.html` at `/f` (from `localhost`)
    Frame,
    /// [`Mode::Frame`], `/f` under `Content-Security-Policy: script-src 'unsafe-inline'`
    FrameCsp,
    /// `errors-page.html` at `/`
    Errors,
    /// [`Mode::Frame`] from `host-page-wrapping.html`, which wraps its own `queueMicrotask` and `setTimeout`
    FrameWrapping,
}

fn serve(s: std::net::TcpStream, mode: Mode, probe: &'static str) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or("GET").to_string();
        let target = parts.next().unwrap_or("/").to_string();
        let mut length = 0usize;
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            if h.trim_end().is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                if k.eq_ignore_ascii_case("content-length") {
                    length = v.trim().parse().unwrap_or(0);
                }
            }
        }
        if length > 0 {
            let mut body = vec![0u8; length];
            if r.read_exact(&mut body).is_err() {
                return;
            }
        }
        let mut extra = String::new();
        let body: String = match (method.as_str(), target.as_str(), mode) {
            ("POST", _, _) => String::new(),
            (_, "/", Mode::Page) => PROBE_PAGE.replace("PROBE", probe),
            (_, "/", Mode::Errors) => ERRORS_PAGE.to_string(),
            (_, "/", Mode::Frame | Mode::FrameCsp) => HOST_PAGE.to_string(),
            (_, "/", Mode::FrameWrapping) => WRAPPING_HOST_PAGE.to_string(),
            (_, "/f", Mode::Frame | Mode::FrameCsp | Mode::FrameWrapping) => {
                if mode == Mode::FrameCsp {
                    extra.push_str("Content-Security-Policy: script-src 'unsafe-inline'\r\n");
                }
                FRAME_PAGE.replace("PROBE", probe)
            }
            _ => String::new(),
        };
        let status = if method == "POST" { "200 OK" } else if body.is_empty() { "404 Not Found" } else { "200 OK" };
        let out = format!("HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n{extra}Content-Length: {}\r\n\r\n{body}", body.len());
        if w.write_all(out.as_bytes()).is_err() {
            return;
        }
    }
}

/// What the page reports, its ports read `PORT`.
pub(crate) async fn answers(mode: Mode, probe: &'static str) -> serde_json::Value {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || serve(s, mode, probe));
        }
    });
    let url = format!("http://127.0.0.1:{port}/");
    let p = persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the Chrome 152 bundle");
    let mut e = engine::Engine::new(Box::new(p));
    e.set_url(&url);
    let resp = transport::fetch(transport::FetchRequest::navigation(url.clone()).worn_by(e.persona().as_ref())).await.expect("the navigation");
    e.load_page_response(&resp.bytes, &resp.url, &[], &resp.headers).await.expect("load");
    let start = Instant::now();
    let got = loop {
        assert!(start.elapsed() < Duration::from_secs(30), "the page never reported");
        let _ = e.pump(Duration::from_millis(50)).await;
        let v = e.eval_value("r", "String(window.__report || '')".to_string()).expect("eval");
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            break s.to_string();
        }
    };
    serde_json::from_str(&ports_read_port(&got)).expect("the page's report")
}

/// `:` and two to five digits, where no digit follows, read `:PORT` (the recording's normalisation).
fn ports_read_port(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b':' {
            let n = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
            if (2..=5).contains(&n) && !b.get(i + 1 + n).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
                out.push_str(":PORT");
                i += 1 + n;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Row by row, so a failure names the rows that differ.
pub(crate) fn assert_as_chrome(got: serde_json::Value, chrome: &str) {
    let want: serde_json::Value = serde_json::from_str(chrome).expect("the recorded Chrome answers");
    if got == want {
        return;
    }
    let (g, w) = (got.as_array().cloned().unwrap_or_default(), want.as_array().cloned().unwrap_or_default());
    let mut rows = String::new();
    for i in 0..g.len().max(w.len()) {
        if g.get(i) != w.get(i) {
            rows.push_str(&format!("\n  korvus {}\n  Chrome {}", g.get(i).map(|v| v.to_string()).unwrap_or_default(), w.get(i).map(|v| v.to_string()).unwrap_or_default()));
        }
    }
    panic!("differs from Chrome 150:{rows}");
}

/// Filled and stroked canvas text, at the alphabetic and the top baseline and half a pixel off: Chrome 150 on Linux
/// outlines every system font with skrifa (Skia's Fontations backend) and rasterizes the path, hinted as the font
/// service names the strike (`sans-serif` is Noto Sans CJK KR here, slightly hinted). This engine's FILLED glyphs were
/// FreeType's own masks: `16px Arial` "Hwmqxli" differed in 201 of 382 pixels, while the same text stroked -- Skia's
/// own path rasterizer on both sides -- did not. On an opaque canvas the text is LCD, and so is a stroke.
#[tokio::test]
async fn canvas_text_is_chromes_fontations_glyphs() {
    let got = answers(Mode::Page, include_str!("fixtures/canvas-and-errors/canvas-text-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/canvas-and-errors/canvas-text-chrome-152.json"));
}

/// a bot challenge's own 48x48 scene (from its trace), on a canvas with alpha and an opaque one: the glyphs
/// are outlined at the text size times the transform's scale RELAXED to 1/1024 (`sk_relax`) and placed at the exact
/// transform, the gradient shader and the shadow kept as they are.
#[tokio::test]
async fn the_challenges_canvas_scene_as_chrome_150_draws_it() {
    let got = answers(Mode::Page, include_str!("fixtures/canvas-and-errors/canvas-scene-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/canvas-and-errors/canvas-scene-chrome-152.json"));
}

/// The canvas `filter` functions as Blink's `FilterEffectBuilder` makes them: `grayscale` and `sepia` matrices of the
/// Filter Effects coefficients worked in double, `saturate` and `hue-rotate` in `FEColorMatrix`'s float formulas,
/// `invert` / `opacity` / `brightness` / `contrast` as 256-entry component-transfer tables. `grayscale(1)` was a
/// saturate matrix of the rounded coefficients here, a byte off on coloured pixels; the four transfers were matrices.
#[tokio::test]
async fn canvas_filters_as_blink_builds_them() {
    let got = answers(Mode::Page, include_str!("fixtures/canvas-and-errors/canvas-filters-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/canvas-and-errors/canvas-filters-chrome-152.json"));
}

/// `getContext('2d', {alpha: false})`: an opaque backing, opaque black until drawn on, a translucent fill composited
/// onto it, read back opaque after `clearRect` and a `copy`, reset to black by a resize. This engine ignored `alpha`.
#[tokio::test]
async fn an_opaque_canvas_as_chrome_150s() {
    let got = answers(Mode::Page, include_str!("fixtures/canvas-and-errors/canvas-opaque-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/canvas-and-errors/canvas-opaque-chrome-152.json"));
}

/// `drawImage` anti-aliases only under a rotation or a skew or for a destination under one device pixel (Blink's
/// `ShouldDrawImageAntialiased`): a 1:1 draw at a half pixel has hard edges. This engine anti-aliased every draw.
#[tokio::test]
async fn draw_image_antialiases_as_blink_decides() {
    let got = answers(Mode::Page, include_str!("fixtures/canvas-and-errors/canvas-image-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/canvas-and-errors/canvas-image-chrome-152.json"));
}

/// Images sampled as Chrome 150 samples them: drawn smaller, larger, unevenly and through the transform, from a
/// sub-rectangle and as a pattern, at every `imageSmoothingQuality` and with smoothing off; the challenge's 80x24
/// text strip read back at 1x1 and its arcs. Three things had to hold at once. The Skia is Chrome's: its revision
/// built with Chrome's defines (scripts/skia-source.sh), where an N32 image drawn source-over is sampled through the
/// legacy shader context and an oval's edge is chopped the legacy way. The image is N32, as Chrome's decoded images
/// and canvas snapshots are (an RGBA one went through the raster pipeline: the 8x1 strip drawn 3 px wide read
/// 213/148/102 where Chrome reads 207/147/98). And the sampling is the one cc's raster derives from the recorded
/// quality and the device matrix (`FilterQualityToSkSamplingOptions` with `MatrixToScalingOperation`): medium is
/// trilinear, high is Mitchell's cubic only when both axes grow, a pattern never cubic; this engine sampled medium
/// from the nearest mip level and high trilinear when upscaling.
#[tokio::test]
async fn images_are_sampled_as_chrome_150_samples_them() {
    let got = answers(Mode::Page, include_str!("fixtures/canvas-and-errors/canvas-sampling-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/canvas-and-errors/canvas-sampling-chrome-152.json"));
}

/// `window.onerror` on the page: the column of the `throw`, not of the `new Error`; a thrown object's message is
/// `"Uncaught " + String(object)` (`[object Object]`, as V8's `ReportMessage` converts it before Blink sees it), where
/// this engine wrote V8's own `#<Object>`.
#[tokio::test]
async fn onerror_on_a_page_as_chrome_150() {
    let got = answers(Mode::Page, include_str!("fixtures/canvas-and-errors/onerror-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/canvas-and-errors/onerror-page-chrome-152.json"));
}

/// `window.onerror` -- and a `<body onerror>` -- in a cross-site frame get the five arguments, as on the page. This
/// engine called a frame's with the `ErrorEvent` alone (the special handling looked for the PAGE's window and the
/// page's `ErrorEvent` constructor).
#[tokio::test]
async fn onerror_in_a_cross_site_frame_as_chrome_150() {
    let got = answers(Mode::Frame, include_str!("fixtures/canvas-and-errors/onerror-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/canvas-and-errors/onerror-frame-chrome-152.json"));
}

/// Uncaught errors are reported where they were THROWN -- in a classic script, a timer, a string timer, a listener
/// and a microtask alike, the Error made a line earlier -- and a `queueMicrotask` callback's throw is reported at all
/// (this engine put the bare callback on V8's queue, whose runner reported it to no one). Stock Chrome without
/// DevTools: an inspector makes V8 capture stacks for uncaught exceptions, and Blink then names the construction.
#[tokio::test]
async fn uncaught_errors_report_where_they_were_thrown_as_chrome_150() {
    let got = answers(Mode::Errors, "").await;
    assert_as_chrome(got, include_str!("fixtures/canvas-and-errors/errors-chrome-152.json"));
}

/// A frame's string timer is a script of no resource: its frames read `<anonymous>:1:16` and its error's file is the
/// frame's document. This engine compiled it under the document's URL.
#[tokio::test]
async fn a_frames_string_timer_is_an_anonymous_script_as_chrome_150() {
    let got = answers(Mode::Frame, include_str!("fixtures/canvas-and-errors/string-timer-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/canvas-and-errors/string-timer-frame-chrome-152.json"));
}

/// Under the frame's own `script-src` without `'unsafe-eval'`, its string timers answer 0 and never run; a function
/// still schedules. This engine ran a frame's string timers whatever its policy said.
#[tokio::test]
async fn a_frames_string_timers_obey_its_csp_as_chrome_150() {
    let got = answers(Mode::FrameCsp, include_str!("fixtures/canvas-and-errors/string-timer-csp-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/canvas-and-errors/string-timer-csp-chrome-152.json"));
}

/// `new Image(3, 4)` calls neither the page's `document.createElement` nor its `width` / `height` setters (Blink sets
/// the attributes itself). This engine called all three.
#[tokio::test]
async fn the_image_factory_calls_nothing_a_page_can_wrap_as_chrome_150() {
    let got = answers(Mode::Page, include_str!("fixtures/canvas-and-errors/image-factory-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/canvas-and-errors/image-factory-chrome-152.json"));
}

/// A cross-site frame's `queueMicrotask` is its realm's own: a page that wraps its `window.queueMicrotask` (and its
/// `setTimeout`) sees none of the frame's calls. This engine sent a frame's microtasks through the PAGE's current
/// `queueMicrotask`.
#[tokio::test]
async fn a_frames_microtasks_pass_through_nothing_its_page_wrapped_as_chrome_150() {
    let got = answers(Mode::FrameWrapping, include_str!("fixtures/canvas-and-errors/frame-microtask-probe.js")).await;
    assert_as_chrome(got, include_str!("fixtures/canvas-and-errors/frame-microtask-chrome-152.json"));
}
