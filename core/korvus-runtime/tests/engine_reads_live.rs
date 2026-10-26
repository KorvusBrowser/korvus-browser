//! LIVE gate: the engine never calls a page-visible accessor or method on the page's behalf.
//!
//! The probe wraps EVERY configurable method and accessor of every interface prototype on the
//! window, then runs ordinary operations -- 2D and WebGL drawing on `<canvas>` and `OffscreenCanvas`,
//! `putImageData`/`createImageData`, `createObjectURL` + a `blob:` Worker, listeners on nodes, the
//! document and the window, inserted scripts (text and child-node source), a frame, custom-element
//! insertion, `<output>`'s value, a `StaticRange` -- and records every wrapped call made from INSIDE
//! another wrapped call. In Chrome that list is empty: Blink reads its own state. This engine read
//! `Blob.type`, `HTMLCanvasElement.width`/`height`, `WebGLRenderingContext.canvas`, `ImageData`'s size,
//! `Node.nodeType`/`nodeName`/`childNodes`/`data` and `Element.tagName` through those getters, and a
//! page that wraps them -- a bot-challenge frame does -- counted every read
//! (the live call log beside stock Chrome's).
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-08**: the script
//! `fixtures/engine-reads/probe.js`, served from 127.0.0.1, answered `[]`
//! (`fixtures/engine-reads/chrome-152.json`).

#[tokio::test]
async fn the_engine_calls_no_page_visible_api_on_the_pages_behalf() {
    let script = include_str!("fixtures/engine-reads/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/engine-reads/chrome-152.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got, want, "calls this engine made through page-visible APIs while running the probe's operations");
}

/// A frame document's parser scripts -- classic and module -- and the scripts inserted into it run from
/// their CHILD text content read off the arena. The frame's step runner read each one's `textContent`
/// through the page-visible getter, and the live a challenge provider challenge frame saw its own 241 KB inline
/// script read that way (a stack-logging hook: the getter's only caller was
/// native). Chrome 150 reads none: `fixtures/frame-script-reads/chrome-152.json`, same probe, same day.
#[tokio::test]
async fn a_frames_scripts_are_not_read_through_text_content() {
    let script = include_str!("fixtures/frame-script-reads/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/frame-script-reads/chrome-152.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got, want, "[reads of textContent/text on the frame's script elements, scripts ran x4]");
}

/// `a.text` is the element's text content, read and written by the engine's own steps (it went through
/// the page-visible `textContent` accessor both ways): its answers, WebIDL `null` -> "null", one childList
/// record per set, against Chrome 150 (`fixtures/anchor-text/chrome-152.json`, same probe, 2026-10-08).
#[tokio::test]
async fn anchor_text_reads_and_writes_as_chrome_150() {
    let script = include_str!("fixtures/anchor-text/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/anchor-text/chrome-152.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got, want);
}

/// The engine's steps never reach what a page can REPLACE either: every interface prototype's methods,
/// getters AND setters wrapped, the window's own members (`origin`, `performance`, `getSelection`,
/// `on<type>`, ...), the global NAMES (an interface object or an ES global the engine looks up by name), and
/// every interface constructor and static -- then tables, forms and labels, clicks and activation, ranges and
/// selections, `document.title`, `importNode`, `XMLSerializer`, observers, `createObjectURL`, history,
/// timers, messages. This engine made 137 such calls: a table's `insertRow` through the
/// page's `createElement` / `appendChild`, a checkbox's activation through its `checked` / `type`, a range's
/// extraction through `cloneNode`, `document.title = ...` through `textContent`, `createObjectURL` through
/// `window.origin`, `new PerformanceObserver` through `window.performance`. Chrome 150 makes none: its three
/// answers are the probe's OWN callbacks (a custom element's constructor run by `createElement`, a submit
/// listener run by `click` / `requestSubmit`), `fixtures/engine-reads/globals-and-tree-chrome-152.json`, same
/// probe, 2026-10-09.
#[tokio::test]
async fn the_engine_reaches_no_global_or_member_a_page_can_replace() {
    let script = include_str!("fixtures/engine-reads/globals-and-tree-probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/engine-reads/globals-and-tree-chrome-152.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got, want, "calls this engine made through what a page can replace, while running the probe's operations");
}

/// The dictionaries the ENGINE builds for the events it fires (and the other objects it constructs) have no
/// prototype: a page that defines getters on `Object.prototype` for `bubbles`, `data`, `loaded`, ... saw every
/// XHR progress event, message, click, focus, `popstate` and `FileReader` event the engine made read them.
/// Chrome 150 reads none: `fixtures/engine-reads/init-dicts-chrome-152.json`, same
/// probe, 2026-10-09.
#[tokio::test]
async fn the_engines_own_dictionaries_read_nothing_from_object_prototype() {
    let script = include_str!("fixtures/engine-reads/init-dicts-probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/engine-reads/init-dicts-chrome-152.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got, want, "[operation, the Object.prototype getters its engine-made dictionaries hit]");
}

/// A Trusted Types sink reads a value of its own union type as itself: Blink's bindings take the TrustedHTML /
/// TrustedScript / TrustedScriptURL off the value and call nothing on it. This engine stringified it -- the page's
/// `@@toPrimitive` and `toString` on every one of the probe's 30 sinks, a bot-challenge frame's
/// `new Worker(policy.createScriptURL(blobURL))` among them. A Trusted value of another
/// type is stringified in both. Chrome 150: `fixtures/engine-reads/trusted-sinks-chrome-152.json`, same probe,
/// 2026-10-09 (its `execCommand('insertHTML')` row left out: this engine has no editing commands).
#[tokio::test]
async fn a_trusted_value_reaches_its_sink_as_itself() {
    let script = include_str!("fixtures/engine-reads/trusted-sinks-probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/engine-reads/trusted-sinks-chrome-152.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got, want, "[sink, what the page's Trusted-type wrappers saw, the sink's effect]");
}

/// A DOM method's arguments as the binding converts them: all of them first and in order -- `setAttribute`'s name
/// before its value, `before()` on a parentless node still stringifying -- and the method's steps after, so a
/// `replaceChildren()` whose argument throws or fails validity keeps the children; a string argument's Text node
/// made in the method's node document, never through the page's `document.createTextNode`; `script.text` and
/// `title.text` set without the page's `textContent` setter; each member's IDL null / undefined rule
/// (`innerText = undefined` is "undefined", `outerHTML = null` "", `srcdoc = null` "null"); `document.write`'s
/// Trusted overloads. Chrome 150: `fixtures/engine-reads/node-args-chrome-152.json`, same probe, 2026-10-09.
#[tokio::test]
async fn dom_arguments_convert_first_and_in_order_as_chrome_150_does() {
    let script = include_str!("fixtures/engine-reads/node-args-probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/engine-reads/node-args-chrome-152.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got, want, "[operation, what the page's wrappers saw, its effect]");
}
