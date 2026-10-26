//! A frame's first documents -- `about:blank` and `about:srcdoc` -- answered as stock Chrome 150 answers them.
//! Each list was recorded from `/usr/bin/google-chrome` 150.0.7871.46 on this machine with the
//! probe beside it in `fixtures/frame-documents/` and is replayed here in the persona's engine; the page's origin
//! reads `ORIGIN` in both.
//!
//! a bot-challenge frame makes an `about:blank` child and reads its `baseURI`: Chrome answers the challenge
//! frame's URL (the creator's base URL, snapshot when the child was made), this engine answered the top page's.

/// The probe's answers in a page at the origin's root, where the recording's page was (`ORIGIN/`).
async fn answers(script: &str) -> Vec<serde_json::Value> {
    let p = persona::CapturedPersona::select(Some(crate::keyboard_live::CHROME_BUNDLE)).expect("the bundle");
    let mut e = engine::Engine::new(Box::new(p));
    let url = "http://127.0.0.1/";
    e.set_url(url);
    e.load_page_response(b"<!doctype html><html><head><meta charset=utf-8></head><body></body></html>", url, &[], &[]).await.expect("load");
    let got = e
        .eval_value_settled("r", format!("Promise.resolve({script}).then(r => JSON.stringify(r))"), std::time::Duration::from_secs(10))
        .await
        .expect("eval");
    serde_json::from_str(got.as_str().unwrap_or("[]")).expect("a list")
}

async fn differences(probe: &str, chrome: &str) -> Vec<String> {
    let want: Vec<serde_json::Value> = serde_json::from_str(chrome).expect("the recorded Chrome list");
    let got = answers(probe).await;
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect()
}

/// A `srcdoc` navigates as Chrome's does, a task after the frame is inserted: the initial `about:blank` document
/// stays (`complete`, an empty body) until the commit, which lands after the 0 ms timers queued in the same task --
/// two frames appended together commit after a `setTimeout(0)` queued after them; the frame's `Window` is kept for
/// that first navigation. And an `about:` document's base URL is its creator's (its `<base>` included) as it was when
/// the frame was made, then its own `<base>` against that: a blank child of a srcdoc with `<base
/// href="https://creator.example/dir/">` resolves against `https://creator.example/dir/`.
#[tokio::test]
async fn srcdoc_frames_commit_and_resolve_as_chrome_150s_do() {
    let d = differences(include_str!("fixtures/frame-documents/srcdoc-timing-and-base-probe.js"), include_str!("fixtures/frame-documents/srcdoc-timing-and-base-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// `srcdoc` set by property, by `setAttribute` and removed: each navigates, asynchronously -- a removal to the
/// frame's `src`, or to `about:blank` when it has none -- and a superseded navigation never commits.
#[tokio::test]
async fn srcdoc_navigations_follow_the_attribute_as_chrome_150s_do() {
    let d = differences(include_str!("fixtures/frame-documents/srcdoc-navigation-probe.js"), include_str!("fixtures/frame-documents/srcdoc-navigation-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// An `about:srcdoc` and an `about:blank` document's URLs, base URLs, origins, referrers and modes, synchronously
/// and once loaded, and `getComputedStyle` of an element whose frame is gone (an empty declaration).
#[tokio::test]
async fn about_documents_report_their_urls_as_chrome_150s_do() {
    let d = differences(include_str!("fixtures/frame-documents/about-urls-probe.js"), include_str!("fixtures/frame-documents/about-urls-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// `getComputedStyle` of an element in a frame that was removed: an empty declaration -- no length, `''` for every
/// value, nothing indexed -- live, so one taken before the removal reads empty after it.
#[tokio::test]
async fn a_removed_frames_computed_style_is_empty_as_in_chrome_150() {
    let d = differences(include_str!("fixtures/frame-documents/removed-frame-style-probe.js"), include_str!("fixtures/frame-documents/removed-frame-style-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// A frame document's `document.write`/`open`/`close`, served from a real origin: a `write()` with no `open()`
/// opens the SAME Document (emptied, its listeners erased, `loading`, the entry document's URL) and puts the markup
/// in at once -- a bot-challenge frame writes `<html><head></head><body></body></html>` into a blank child
/// and reads 400 properties of its body's computed style, "" each here, where Chrome answers them; `close()` runs the
/// document's end before it returns (`interactive`, DOMContentLoaded, `complete`, the window's `load`, the element's);
/// `close()` with nothing open is nothing; and a navigated frame's window fires `load` before its element does.
#[tokio::test]
async fn frame_document_write_open_and_close_as_chrome_150s_do() {
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/frame-documents/document-write-chrome-150.json")).expect("the recorded Chrome list");
    let got = crate::xhr_streaming_live::served_answers(include_str!("fixtures/frame-documents/document-write-probe.js")).await;
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    let d: Vec<String> = got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect();
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// A frame's `first-paint` and `first-contentful-paint`: drawn by its FLAT tree -- text or an SVG inside an open or
/// closed shadow root, a host's text a slot shows, not a host's text no slot shows -- and presented only once the
/// page's paint holding has ended (a page whose body draws nothing holds its commits up to 500 ms, and a frame's
/// drawing reaches the screen through them). a bot-challenge frame draws its widget inside a closed shadow
/// root: Chrome's timeline went on to `paint`, this engine's never did.
#[tokio::test]
async fn frame_paint_entries_follow_the_flat_tree_and_the_pages_hold_as_in_chrome_150() {
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/frame-documents/paint-entries-chrome-150.json")).expect("the recorded Chrome list");
    let got = crate::xhr_streaming_live::served_answers(include_str!("fixtures/frame-documents/paint-entries-probe.js")).await;
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    let d: Vec<String> = got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect();
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// A frame document's end, same-origin (`127.0.0.1`) and cross-origin (`localhost`) alike: `DOMContentLoaded`, its
/// handlers' promise jobs, then -- in the same task, with nothing left to load (Blink's `FinishedParsing` and
/// `CheckCompleted`) -- `complete` and the window's `load`, so a 0 ms timer set by a `DOMContentLoaded` handler reads
/// `complete` and a `loadEventEnd`; the `<iframe>`'s own `load` reaches the parent before anything the frame posts
/// from that timer. a bot-challenge frame's handler reads its navigation entry's `duration` there: Chrome
/// 1006 ms, this engine 0, its load a task later.
#[tokio::test]
async fn a_frame_documents_load_follows_its_dom_content_loaded_as_in_chrome_150() {
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/frame-documents/load-steps-chrome-150.json")).expect("the recorded Chrome list");
    let got = crate::xhr_streaming_live::served_answers(include_str!("fixtures/frame-documents/load-steps-probe.js")).await;
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    let d: Vec<String> = got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect();
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// A frame document reopened FROM ITS OWN `load` fires no second `load`: an `iframe.onload` that writes and closes
/// the frame's document runs once, and the document `close()` ends is `complete` -- Blink keeps the load event in
/// progress across an `open()` made while it is dispatched (`ImplicitOpen`, `ShouldComplete`). The same write
/// and close from a later task does fire `load` again, before `close()` returns. Here the handler ran again inside
/// `close()`, and again, until WPT's `query-encoding/*?include=base-href` overflowed the stack
/// (Chrome 150, three runs alike).
#[tokio::test]
async fn a_frame_document_reopened_from_its_load_fires_no_second_load_as_in_chrome_150() {
    let d = differences(include_str!("fixtures/frame-documents/close-in-load-probe.js"), include_str!("fixtures/frame-documents/close-in-load-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}

/// A frame document's own parser script writes at the insertion point, as Chrome 152's does: what it writes is in the
/// tree, after the script, before the write returns; a written inline script runs; a written external script runs
/// once loaded. The writes were buffered for a `close()` that never came: Naver's ad frames, whose
/// `gfp-display-safeframe.js` writes the creative this way, stayed empty and 0px tall.
#[tokio::test]
async fn a_frame_parser_scripts_write_goes_in_at_the_insertion_point_as_in_chrome_152() {
    let d = differences(include_str!("fixtures/frame-documents/parser-script-write-probe.js"), include_str!("fixtures/frame-documents/parser-script-write-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 152:\n{}", d.len(), d.join("\n"));
}

/// `open()` / `write()` / `close()` on a Document no browsing context shows, as Chrome 150's script-created parser
/// answers: korvus wrote nothing into a `createHTMLDocument` or `DOMParser` document. An
/// `open()` empties it and the writes build it; a write after `close()` opens it again; and a `createHTMLDocument`
/// result is written to without one -- Blink never closes the parser that made it, so the write lands in its
/// `<body>`. An XML document refuses in Blink's words.
#[tokio::test]
async fn a_script_made_documents_parser_writes_as_chrome_150s_does() {
    let d = differences(include_str!("fixtures/frame-documents/created-document-write-probe.js"), include_str!("fixtures/frame-documents/created-document-write-chrome-152.json")).await;
    assert!(d.is_empty(), "{} answers differ from Chrome 150:\n{}", d.len(), d.join("\n"));
}
