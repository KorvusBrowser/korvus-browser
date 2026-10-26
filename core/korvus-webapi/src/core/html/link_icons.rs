//! `core/html/link_icons.rs` -- the icons a browser requests FOR the page: the favicon, and the icon a
//! web-app manifest offers for installation (L17's E2).
//!
//! THE DEFECT. `<link rel=icon | shortcut icon | apple-touch-icon>` went through the element loader, so
//! every candidate was requested, at parse time, and a page that declared no icon requested nothing.
//! None of that is how a Chromium browser behaves, because in Chromium the PAGE never loads an icon:
//! the renderer sends the candidate list to the browser process once the load event has finished
//! (`LocalFrame::UpdateFaviconURL`, from `ProgressTracker::ProgressCompleted`, and again whenever an
//! icon `<link>` changes after that), and the browser's favicon handler picks and downloads.
//!
//! Measured on Chrome 150.0.7871.46 over a loopback server's access log; headless and headed under
//! Xvfb agree on every row, and DPR 2 changes none:
//!
//! ```text
//! no icon link                                        /favicon.ico
//! apple-touch-icon x2 (+ -precomposed), no icon       /favicon.ico -- a touch icon is not a favicon
//! mask-icon only                                      /favicon.ico
//! <link rel=icon> in <body>                           /favicon.ico -- only <head>'s children count
//! icon media="(max-width: 100px)" (no match)          /favicon.ico
//! a manifest with no icons                            /favicon.ico as well
//! rel=icon sizes 16 + 32 (real PNGs)                  r32 only
//! rel=icon sizes 16,32,48,96,192                      r32 only
//! sizes 16,32,192 + sizes=any svg                     the svg only
//! rel=icon 48 + 16                                    r16 only
//! rel=icon 192 + 512                                  r192 only
//! two icons with no sizes (32x32 PNGs)                both, the LAST declared first
//! icon (no sizes, 16px) + icon sizes=32               the no-sizes one, then r32
//! rel="alternate icon" png + rel=icon svg             both (the svg first)
//! rel=icon type=image/x-foo                           fetched -- `type` is not consulted
//! rel=icon to a 404                                   that URL only; no /favicon.ico fallback
//! after load: icon inserted, then its href changed    each new URL, once
//! timing                                              1-7 ms AFTER `load`, never delaying it
//! CSP `default-src 'none'` (or `img-src 'none'`)      NOTHING -- see `core/html/csp.rs` (P14b)
//! ```
//!
//! # The model
//!
//! Candidates (Blink `Document::IconURLs`): the `<link>` children of `<head>` with an `icon` token in
//! `rel`, a non-empty `href` and a matching `media`, LAST declared first; none at all, on an http(s)
//! document, is `/favicon.ico`. The browser (`FaviconHandler`) scores each candidate's declared
//! `sizes` against the pixel sizes it wants -- 16 CSS px at scale 1 and 2, i.e. `{16, 32}`, the same at
//! DPR 1 and 2 as measured -- with Chromium's `SelectFaviconFrameIndices` scoring (exact 1; larger
//! `0.15 + 0.01·want/size`; smaller `0.1 + 0.01·size/want`; eight times too big `0.01·want/size`;
//! averaged over the wanted sizes), a candidate with no usable `sizes` scoring 1. It downloads in
//! score order (stable), ONE AT A TIME, and scores each image it gets by the sizes it ACTUALLY has; it
//! moves to the next candidate only while that candidate's declared score is higher than the best
//! image so far. Every row of the table above follows from those four sentences, including the two
//! that look odd (`48 + 16` takes 16: 0.5525 beats 0.155; two unsized 32s both load: 0.5775 < 1).
//!
//! # Rulings
//!
//! * The actual size is read from the image's HEADER (PNG, ICO, GIF, BMP, WebP), and an SVG counts as
//!   one bitmap at the largest wanted size. Nothing is decoded. A body whose header is not an image
//!   counts as a failed download. The subresource loader (`spec::SubresourceResponse`) hands this file text, not bytes, so a header byte at
//!   or above 0x80 is unknown and taken as 0xC0. Cost if wrong: a 128-255 px icon's score is off in
//!   the fourth decimal.
//! * An image whose format has no parseable header here (JPEG) counts as decoded at its declared
//!   score. Cost if wrong: one more or one fewer favicon candidate requested on a JPEG-favicon page.
//! * Only the page's own document has icons; a nested frame's `<link rel=icon>` is ignored, as
//!   `LocalFrame::UpdateFaviconURL` ignores a non-main frame.
//! * An icon download is subject to the document's CSP `img-src`. A blocked candidate
//!   is treated as a failed download -- no request, no score -- and a blocked default `/favicon.ico`
//!   leaves the page asking for nothing at all, which is what `man.openbsd.org` and matter-js's 404
//!   made Chrome do and this engine did not. The measured table is `core/html/csp.rs`.
//!
//! # The manifest's icon
//!
//! Chrome also downloads ONE icon from the page's web-app manifest -- the installability check's
//! primary icon -- and only when the manifest could make the page installable. Measured, one manifest
//! per row, `start_url: "."` and `display: standalone` unless the row says otherwise:
//!
//! ```text
//! name + icons 192,512                                r192
//! no start_url                                        nothing
//! no name / empty name and short_name                 nothing
//! display browser (or absent)                         nothing; display_override [standalone] → r192
//! display fullscreen, icons 180,256                   r180;   minimal-ui, 192+512 → r192
//! icons 144,512 → r144 · 512 → r512 · 256,1024 → r256 · 96 or 100,143 → nothing
//! purpose maskable → nothing · "any maskable" → r192
//! no sizes → nothing · sizes any (svg) → the svg · "192x192 512x512" → it · 192x100 + 200 → r200
//! type image/webp → it · type image/foo → nothing
//! start_url on another origin → nothing · a relative start_url → r192
//! ```
//!
//! So: a named manifest with a same-origin `start_url` and an installable display mode; among its
//! `purpose: any` icons of a supported type, the square one of exactly 144 px, else the smallest
//! larger, else one of size `any`. 144 is Chromium's minimum installable icon size on desktop.

use std::collections::HashMap;

use deno_core::{v8, OpState};

use crate::binding::protos::build_fn_data;
use crate::binding::HostDom;

/// The pixel sizes the favicon handler wants: 16 CSS px at scale factors 1 and 2 (module doc).
const FAVICON_WANTED: [u32; 2] = [16, 32];

/// Chromium's desktop installability icon: ideal and minimum size, in px (module doc).
const MANIFEST_ICON_PX: u32 = 144;

#[derive(Clone, Debug, PartialEq)]
struct IconCandidate {
    url: String,
    sizes: Vec<(u32, u32)>,
}

#[derive(Default)]
pub(crate) struct Icons {
    /// The document's `load` event has fired.
    loaded: bool,
    /// The candidate list the handler last acted on -- an identical list is not a new request.
    last: Option<Vec<IconCandidate>>,
    /// What a URL's download scored (`None` = failed), so a later list does not re-request it.
    downloaded: HashMap<String, Option<f32>>,
    /// The download sequence in progress: its candidates in order, the next index, the best score.
    queue: Vec<(IconCandidate, f32)>,
    next: usize,
    best: f32,
    generation: u32,
    update_queued: bool,
    /// The first `<link rel=manifest>`'s parsed body and URL, once it arrived.
    manifest: Option<(serde_json::Value, String)>,
    manifest_icon_requested: bool,
    epoch: u32,
    /// The document was LEFT before the handover: a navigation had already been asked
    /// for at the microtask checkpoint that follows the load event, so this document never hands a
    /// candidate list over at all. See [`document_loaded`].
    abandoned: bool,
}

fn with<R>(st: &mut OpState, f: impl FnOnce(&mut Icons) -> R) -> R {
    if st.try_borrow::<Icons>().is_none() {
        st.put(Icons::default());
    }
    f(st.borrow_mut::<Icons>())
}

fn with_scope<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut Icons) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    with(&mut st, f)
}

/// A new document.
pub(crate) fn reset(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>) {
    let mut st = op_state.borrow_mut();
    with(&mut st, |i| {
        let epoch = i.epoch.wrapping_add(1);
        let generation = i.generation.wrapping_add(1);
        *i = Icons { epoch, generation, ..Icons::default() };
    });
}

/// Does a `rel` value make a `<link>` a favicon? Blink `LinkRelAttribute`: an `icon` token, ASCII
/// case-insensitively, among whitespace-separated tokens (`shortcut icon`, `alternate icon`, ...).
pub(crate) fn is_icon_rel(rel: &str) -> bool {
    rel.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("icon"))
}

/// The page document's `load` event has fired: the favicon handler gets its first candidate list.
/// NOT INSIDE THE LOAD TASK. Chrome's list crosses to the browser process and the favicon handler
/// consults its history database before it downloads, so the request goes out on a later task (1-7 ms
/// after `load`, measured) -- never ahead of what the page's own `onload` handler started. It is
/// started here on the next turn of the event loop for the same reason, and that ordering is
/// observable: issued synchronously from the load step, the icon request raced the scripts an
/// `onload` handler had just inserted, and `execution-timing/078.html` (whose runner serves no
/// `trickle` delay, so its two scripts race) flipped order in 4 of 7 full WPT runs; with no icon
/// request it passed 1 of 1, and deferred as here 2 of 2.
///
/// # A document that is LEFT never hands its list over
///
/// Blink calls `LocalFrame::UpdateFaviconURL` from `ProgressTracker::ProgressCompleted`, which is the
/// end of the load step -- so a navigation that starts before that point means no icon is ever
/// requested for the document, and one that starts after it does not stop the download. Measured on
/// stock Chrome 150.0.7871.46, over a loopback server's access log, with the page beaconing its own
/// `load` and its own navigation so the two orders are read rather than assumed:
///
/// ```text
/// a page that never navigates                          load, then /favicon.ico
/// a script navigates DURING PARSE                      `load` never fires; no icon
/// a `load` handler navigates SYNCHRONOUSLY             load, navigation; no icon
/// a MICROTASK queued in the `load` handler navigates   load, navigation; no icon
/// `setTimeout(..., 0 | 4 | 12)` in it navigates        load, navigation AND /favicon.ico
/// ```
///
/// So the boundary is the microtask checkpoint that follows the load event, and it is read there:
/// [`cb_navigation_checkpoint`] runs as a microtask enqueued after the load handlers have returned --
/// behind any microtask they queued, and ahead of any task they queued -- and records whether a
/// top-level navigation has been asked for. The deferred handover below honours that bit.
pub(crate) fn document_loaded(scope: &mut v8::PinScope) {
    let epoch = with_scope(scope, |i| {
        i.loaded = true;
        i.epoch
    });
    let check = build_fn_data(scope, cb_navigation_checkpoint, "", 0, v8::undefined(scope).into());
    scope.enqueue_microtask(check);
    let Some(f) = crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_css_frame") else {
        update(scope);
        manifest_check(scope);
        return;
    };
    let ms: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, 0).into();
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let Some(p) = f.call(scope, undef, &[ms]).and_then(|v| v8::Local::<v8::Promise>::try_from(v).ok()) else {
        update(scope);
        manifest_check(scope);
        return;
    };
    let data: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, epoch).into();
    let go = build_fn_data(scope, cb_after_load, "", 1, data);
    p.then2(scope, go, go);
}

fn cb_after_load(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(epoch) = crate::binding::protos::body_data(scope, &args).uint32_value(scope) else { return };
    if with_scope(scope, |i| i.epoch) != epoch {
        return;
    }
    update(scope);
    manifest_check(scope);
}

/// The microtask checkpoint after the load event: has a script asked to leave this document?
/// A `javascript:` URL is not a navigation (`PendingNavigation` carries those too, and the drivers
/// filter them out the same way).
fn cb_navigation_checkpoint(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let leaving = st
        .try_borrow::<crate::core::html::browsing_context::PendingNavigation>()
        .and_then(|n| n.0.as_ref())
        .is_some_and(|n| !n.url.trim_start().get(..11).is_some_and(|s| s.eq_ignore_ascii_case("javascript:")));
    if leaving {
        with(&mut st, |i| i.abandoned = true);
    }
}

/// An icon `<link>` was inserted or changed. Before `load` nothing happens (the list is read at load);
/// after it, the list is re-read at the next microtask checkpoint.
pub(crate) fn link_changed(scope: &mut v8::PinScope) {
    let go = with_scope(scope, |i| i.loaded && !std::mem::replace(&mut i.update_queued, true));
    if !go {
        return;
    }
    let f = build_fn_data(scope, cb_queued_update, "", 0, v8::undefined(scope).into());
    scope.enqueue_microtask(f);
}

fn cb_queued_update(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    with_scope(scope, |i| i.update_queued = false);
    update(scope);
}

/// Blink's `WebIconSizesParser::ParseIconSizes`: `WxH` tokens and `any` (0x0), anything else skipped.
fn parse_sizes(s: &str) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for tok in s.split_ascii_whitespace() {
        if tok.eq_ignore_ascii_case("any") {
            out.push((0, 0));
            continue;
        }
        let Some((w, h)) = tok.split_once(['x', 'X']) else { continue };
        let ok = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) && !t.starts_with('0');
        if ok(w) && ok(h) {
            if let (Ok(w), Ok(h)) = (w.parse(), h.parse()) {
                out.push((w, h));
            }
        }
    }
    out
}

/// Chromium `GetCandidateIndexWithBestScore` + `GetCandidateIndicesWithBestScores`: how well a set of
/// bitmap sizes serves the wanted pixel sizes.
fn frames_score(sizes: &[(u32, u32)]) -> f32 {
    let mut total = 0.0;
    for want in FAVICON_WANTED {
        let want = want as f32;
        let mut best = 0.0f32;
        for &(w, h) in sizes {
            let (w, h) = (w as f32, h as f32);
            if w == want && h == want {
                best = 1.0;
                break;
            }
            let avg = (w + h) / 2.0;
            let huge = want * 8.0;
            let s = if w >= huge || h >= huge {
                (want / avg).min(1.0) * 0.01
            } else if w >= want && h >= want {
                want / avg * 0.01 + 0.15
            } else {
                (avg / want).min(1.0) * 0.01 + 0.1
            };
            best = best.max(s);
        }
        total += best;
    }
    total / FAVICON_WANTED.len() as f32
}

/// A candidate's DECLARED score: 1 with no usable `sizes` (nothing known), else the frames score of the
/// sizes it names.
fn declared_score(c: &IconCandidate) -> f32 {
    let sized: Vec<(u32, u32)> = c.sizes.iter().copied().filter(|&(w, h)| w > 0 && h > 0).collect();
    if sized.is_empty() {
        1.0
    } else {
        frames_score(&sized)
    }
}

/// The bytes of a text body: an ASCII or valid multi-byte character is its own bytes; U+FFFD -- an
/// invalid byte the transport replaced -- is unknown.
fn body_bytes(body: &str, limit: usize) -> Vec<Option<u8>> {
    let mut out = Vec::with_capacity(limit.min(body.len()));
    for c in body.chars() {
        if out.len() >= limit {
            break;
        }
        if c == '\u{FFFD}' {
            out.push(None);
        } else {
            let mut buf = [0u8; 4];
            out.extend(c.encode_utf8(&mut buf).bytes().map(Some));
        }
    }
    out
}

/// The bitmap sizes an image body declares in its header, `None` if it is not an image this reads
/// (module doc, first ruling). `Some(vec![])` = an image of unknown size.
fn image_sizes(body: &str, content_type: &str) -> Option<Vec<(u32, u32)>> {
    let head = body.trim_start();
    if content_type.contains("svg") || head.starts_with("<svg") || (head.starts_with("<?xml") && head.contains("<svg")) {
        return Some(vec![(FAVICON_WANTED[1], FAVICON_WANTED[1])]);
    }
    let b = body_bytes(body, 4096);
    let byte = |i: usize| -> Option<u32> { b.get(i).map(|x| u32::from(x.unwrap_or(0xC0))) };
    let is = |i: usize, s: &[u8]| s.iter().enumerate().all(|(k, v)| b.get(i + k).copied().flatten() == Some(*v));
    let be32 = |i: usize| -> Option<u32> { Some(byte(i)? << 24 | byte(i + 1)? << 16 | byte(i + 2)? << 8 | byte(i + 3)?) };
    let le16 = |i: usize| -> Option<u32> { Some(byte(i)? | byte(i + 1)? << 8) };
    if is(1, b"PNG") && is(12, b"IHDR") {
        return Some(vec![(be32(16)?, be32(20)?)]);
    }
    if is(0, &[0, 0, 1, 0]) {
        let n = le16(4)?;
        let mut v = Vec::new();
        for k in 0..n.min(64) as usize {
            let e = 6 + 16 * k;
            let w = byte(e)?;
            let h = byte(e + 1)?;
            v.push((if w == 0 { 256 } else { w }, if h == 0 { 256 } else { h }));
        }
        return (!v.is_empty()).then_some(v);
    }
    if is(0, b"GIF8") {
        return Some(vec![(le16(6)?, le16(8)?)]);
    }
    if is(0, b"BM") {
        return Some(vec![(le16(18)?, le16(22)?)]);
    }
    if is(0, b"RIFF") && is(8, b"WEBP") {
        if is(12, b"VP8X") {
            let w = byte(24)? | byte(25)? << 8 | byte(26)? << 16;
            let h = byte(27)? | byte(28)? << 8 | byte(29)? << 16;
            return Some(vec![(w + 1, h + 1)]);
        }
        if is(12, b"VP8L") {
            let bits = byte(21)? | byte(22)? << 8 | byte(23)? << 16 | byte(24)? << 24;
            return Some(vec![((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)]);
        }
        if is(12, b"VP8 ") {
            return Some(vec![(le16(26)? & 0x3fff, le16(28)? & 0x3fff)]);
        }
        return Some(vec![]);
    }
    if content_type.contains("jpeg") || content_type.contains("jpg") || (b.first() == Some(&None) && b.get(1) == Some(&None) && b.get(2) == Some(&None)) {
        return Some(vec![]);
    }
    None
}

/// Read the candidate list off the page document (Blink `Document::IconURLs`, favicon type only).
fn candidates(st: &OpState) -> Vec<IconCandidate> {
    let doc_url = st.try_borrow::<crate::core::dom::CookieUrl>().map(|c| c.0.clone()).unwrap_or_default();
    let base = crate::core::dom::base_url_of(st);
    let media = crate::core::dom::geometry_queries::media_context(st);
    let mut out = Vec::new();
    if let Some(host) = st.try_borrow::<HostDom>() {
        let dom = &host.0;
        let html = dom.child_elements(dom.root()).into_iter().find(|e| dom.tag_name(*e).is_some_and(|t| t.eq_ignore_ascii_case("html")));
        let head = html.and_then(|h| dom.child_elements(h).into_iter().find(|e| dom.tag_name(*e).is_some_and(|t| t.eq_ignore_ascii_case("head"))));
        if let Some(head) = head {
            for link in dom.child_elements(head) {
                if !dom.tag_name(link).is_some_and(|t| t.eq_ignore_ascii_case("link")) {
                    continue;
                }
                if !dom.get_attribute(link, "rel").is_some_and(is_icon_rel) {
                    continue;
                }
                let Some(href) = dom.get_attribute(link, "href").map(str::trim).filter(|h| !h.is_empty()) else { continue };
                if let Some(m) = dom.get_attribute(link, "media").map(str::trim).filter(|m| !m.is_empty()) {
                    if !page::style::media::MediaQueryList::parse(m, None).matches(&media, &[]) {
                        continue;
                    }
                }
                let Some(url) = url::Url::parse(&base).ok().and_then(|b| b.join(href).ok()) else { continue };
                out.push(IconCandidate { url: url.to_string(), sizes: dom.get_attribute(link, "sizes").map(parse_sizes).unwrap_or_default() });
            }
        }
    }
    out.reverse();
    if out.is_empty() {
        if let Ok(u) = url::Url::parse(&doc_url) {
            if matches!(u.scheme(), "http" | "https") {
                if let Ok(fav) = u.join("/favicon.ico") {
                    out.push(IconCandidate { url: fav.to_string(), sizes: Vec::new() });
                }
            }
        }
    }
    out
}

/// Hand the favicon handler the current candidate list.
fn update(scope: &mut v8::PinScope) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let start = {
        let mut st = op_state.borrow_mut();
        let list = candidates(&st);
        with(&mut st, |i| {
            if !i.loaded || i.abandoned || i.last.as_ref() == Some(&list) {
                return false;
            }
            i.last = Some(list.clone());
            let mut scored: Vec<(IconCandidate, f32)> = list.into_iter().map(|c| {
                let s = declared_score(&c);
                (c, s)
            }).collect();
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            i.queue = scored;
            i.next = 0;
            i.best = 0.0;
            i.generation = i.generation.wrapping_add(1);
            true
        })
    };
    if start {
        step(scope);
    }
}

/// Download the next candidate, if the handler would.
fn step(scope: &mut v8::PinScope) {
    loop {
        enum Next {
            Stop,
            Known(Option<f32>),
            Fetch(String, u32, f32),
        }
        let next = with_scope(scope, |i| {
            let Some((cand, declared)) = i.queue.get(i.next).cloned() else { return Next::Stop };
            if i.next > 0 && declared <= i.best {
                return Next::Stop;
            }
            i.next += 1;
            match i.downloaded.get(&cand.url) {
                Some(known) => Next::Known(*known),
                None => Next::Fetch(cand.url, i.generation, declared),
            }
        });
        match next {
            Next::Stop => return,
            Next::Known(score) => {
                with_scope(scope, |i| i.best = i.best.max(score.unwrap_or(0.0)));
            }
            Next::Fetch(url, generation, declared) => {
                // AN ICON DOWNLOAD IS A FETCH, and the document's CSP `img-src`
                // governs it -- measured, `default-src 'none'` makes Chrome ask for nothing at all,
                // not even the default `/favicon.ico`. A blocked candidate is a FAILED download: it
                // scores nothing, so the next candidate is considered exactly as it is after a 404,
                // and an empty list still contributes no fallback of its own. `core/html/csp.rs`
                // carries the measured table.
                if !crate::core::html::csp::allows_image(scope, &url) {
                    with_scope(scope, |i| {
                        i.downloaded.insert(url, None);
                    });
                    continue;
                }
                if url.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("data:")) {
                    let score = data_url_score(&url, declared);
                    with_scope(scope, |i| {
                        i.downloaded.insert(url.clone(), score);
                        i.best = i.best.max(score.unwrap_or(0.0));
                    });
                    continue;
                }
                // Chrome's page timeline carries the favicon as an `other` resource entry (measured),
                // so the load is named before the loader is called.
                crate::core::timing::resource_timing::note_initiator(scope, 0, "other");
                // The favicon's priority is the browser's, not an image element's: `u=1, i` (Chrome 150 over
                // h2, a loopback page each time and two of three requests of a recorded live visit; the
                // other read `i`).
                crate::core::html::subresource::set_pending_load_shape(
                    scope,
                    crate::core::html::subresource::LoadShape { priority: Some("u=1, i".into()), credentials: "", parent: None },
                );
                let Some(p) = crate::core::html::subresource::start_fetch(scope, &url, "image", "no-cors") else { return };
                let arr = v8::Array::new(scope, 3);
                let g = v8::Integer::new_from_unsigned(scope, generation);
                arr.set_index(scope, 0, g.into());
                let u = crate::binding::protos::str_val(scope, &url);
                arr.set_index(scope, 1, u);
                let d = v8::Number::new(scope, f64::from(declared));
                arr.set_index(scope, 2, d.into());
                let ok = build_fn_data(scope, cb_icon_ok, "", 1, arr.into());
                let err = build_fn_data(scope, cb_icon_err, "", 1, arr.into());
                p.then2(scope, ok, err);
                return;
            }
        }
    }
}

fn data_url_score(url: &str, declared: f32) -> Option<f32> {
    let (meta, payload) = url[5..].split_once(',')?;
    let bytes = if meta.to_ascii_lowercase().ends_with(";base64") {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.decode(payload.trim()).ok()?
    } else {
        payload.as_bytes().to_vec()
    };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let sizes = image_sizes(&text, &meta.to_ascii_lowercase())?;
    Some(if sizes.is_empty() { declared } else { frames_score(&sizes) })
}

fn icon_data(scope: &mut v8::PinScope, data: v8::Local<v8::Value>) -> Option<(u32, String, f32)> {
    let arr = v8::Local::<v8::Array>::try_from(data).ok()?;
    let g = arr.get_index(scope, 0)?.uint32_value(scope)?;
    let u = arr.get_index(scope, 1)?.to_rust_string_lossy(scope);
    let d = arr.get_index(scope, 2)?.number_value(scope)? as f32;
    Some((g, u, d))
}

fn settle(scope: &mut v8::PinScope, generation: u32, url: String, score: Option<f32>) {
    let current = with_scope(scope, |i| {
        i.downloaded.insert(url, score);
        if i.generation != generation {
            return false;
        }
        i.best = i.best.max(score.unwrap_or(0.0));
        true
    });
    if current {
        step(scope);
    }
}

fn cb_icon_ok(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((generation, url, declared)) = icon_data(scope, __body_data) else { return };
    let resp = v8::Local::<v8::Object>::try_from(args.get(0)).ok();
    let status = resp.and_then(|r| crate::binding::protos::get_prop(scope, r, "status")).and_then(|s| s.int32_value(scope)).unwrap_or(0);
    let body = resp
        .and_then(|r| crate::binding::protos::get_prop(scope, r, "body"))
        .filter(|b| !b.is_null_or_undefined())
        .map(|b| b.to_rust_string_lossy(scope))
        .unwrap_or_default();
    let headers = resp
        .and_then(|r| crate::binding::protos::get_prop(scope, r, "headers"))
        .map(|h| crate::core::html::isolation_policy::headers_from_js(scope, h))
        .unwrap_or_default();
    // The icon resource's type is the network stack's (`ResourceResponse::MimeType()`, A/B C5): every `Content-Type` value read.
    let ctype = transport::mime::mime_type_and_charset(&headers).mime;
    let score = if (200..300).contains(&status) {
        image_sizes(&body, &ctype).map(|s| if s.is_empty() { declared } else { frames_score(&s) })
    } else {
        None
    };
    settle(scope, generation, url, score);
}

fn cb_icon_err(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((generation, url, _)) = icon_data(scope, __body_data) else { return };
    settle(scope, generation, url, None);
}

// ── the manifest ─────────────────────────────────────────────────────────────────────────────────

/// A `<link rel=manifest>`'s 2xx body arrived. Only the document's FIRST manifest link counts, as it
/// does for Chromium's manifest manager.
pub(crate) fn manifest_arrived(scope: &mut v8::PinScope, link: u32, body: &str, final_url: &str) {
    let first = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let Some(host) = st.try_borrow::<HostDom>() else { return };
        let dom = &host.0;
        dom.descendant_elements(dom.root())
            .into_iter()
            .find(|e| dom.tag_name(*e).is_some_and(|t| t.eq_ignore_ascii_case("link")) && dom.get_attribute(*e, "rel").is_some_and(|r| r.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("manifest"))))
    };
    if first != Some(link) {
        return;
    }
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else { return };
    with_scope(scope, |i| i.manifest = Some((json, final_url.to_string())));
    manifest_check(scope);
}

/// The installability check's primary icon, if the manifest qualifies (module doc).
fn manifest_icon(json: &serde_json::Value, manifest_url: &str, doc_url: &str) -> Option<String> {
    let str_of = |k: &str| json.get(k).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());
    str_of("name").or_else(|| str_of("short_name"))?;
    let base = url::Url::parse(manifest_url).ok()?;
    let doc = url::Url::parse(doc_url).ok()?;
    let start = base.join(json.get("start_url")?.as_str()?).ok()?;
    if start.origin() != doc.origin() {
        return None;
    }
    const OVERRIDE: [&str; 6] = ["fullscreen", "standalone", "minimal-ui", "window-controls-overlay", "tabbed", "borderless"];
    let display = json
        .get("display_override")
        .and_then(|v| v.as_array())
        .and_then(|a| a.iter().filter_map(|v| v.as_str()).find(|d| OVERRIDE.contains(d)))
        .or_else(|| json.get("display").and_then(|v| v.as_str()).filter(|d| ["fullscreen", "standalone", "minimal-ui", "browser"].contains(d)))
        .unwrap_or("browser");
    if display == "browser" {
        return None;
    }
    let mut exact = None;
    let mut larger: Option<(u32, String)> = None;
    let mut any = None;
    for icon in json.get("icons")?.as_array()? {
        let Some(src) = icon.get("src").and_then(|v| v.as_str()).and_then(|s| base.join(s).ok()) else { continue };
        if let Some(t) = icon.get("type").and_then(|v| v.as_str()).map(|t| t.trim().to_ascii_lowercase()).filter(|t| !t.is_empty()) {
            if !matches!(t.as_str(), "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp" | "image/svg+xml" | "image/x-icon" | "image/vnd.microsoft.icon" | "image/bmp" | "image/avif" | "image/apng") {
                continue;
            }
        }
        let purpose_any = icon.get("purpose").and_then(|v| v.as_str()).map_or(true, |p| p.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("any")));
        if !purpose_any {
            continue;
        }
        for (w, h) in icon.get("sizes").and_then(|v| v.as_str()).map(parse_sizes).unwrap_or_default() {
            if w == 0 && h == 0 {
                any.get_or_insert_with(|| src.to_string());
            } else if w == h && w == MANIFEST_ICON_PX {
                exact.get_or_insert_with(|| src.to_string());
            } else if w == h && w > MANIFEST_ICON_PX && larger.as_ref().map_or(true, |(s, _)| w < *s) {
                larger = Some((w, src.to_string()));
            }
        }
    }
    exact.or(larger.map(|(_, u)| u)).or(any)
}

fn manifest_check(scope: &mut v8::PinScope) {
    let icon = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let doc_url = st.try_borrow::<crate::core::dom::CookieUrl>().map(|c| c.0.clone()).unwrap_or_default();
        with(&mut st, |i| {
            if !i.loaded || i.abandoned || i.manifest_icon_requested {
                return None;
            }
            let (json, url) = i.manifest.as_ref()?;
            let icon = manifest_icon(json, url, &doc_url)?;
            i.manifest_icon_requested = true;
            Some(icon)
        })
    };
    let Some(icon) = icon else { return };
    // The installability icon is an image fetch too, and `img-src` governs it. Measured
    // on `default-src 'self'; img-src 'none'`: Chrome fetches the MANIFEST and neither icon.
    if !crate::core::html::csp::allows_image(scope, &icon) {
        return;
    }
    // Named like the favicon's, so the resource timeline does not file a browser-initiated icon load
    // under `css` (ruling: `other`, unmeasured -- the favicon's measured type).
    crate::core::timing::resource_timing::note_initiator(scope, 0, "other");
    if let Some(p) = crate::core::html::subresource::start_fetch(scope, &icon, "image", "no-cors") {
        // The bytes are not used; a rejection is not a page error.
        let done = build_fn_data(scope, cb_ignore, "", 1, v8::undefined(scope).into());
        p.then2(scope, done, done);
    }
}

fn cb_ignore(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(sizes: &str) -> IconCandidate {
        IconCandidate { url: String::new(), sizes: parse_sizes(sizes) }
    }

    /// The measured preferences of the module doc, as declared scores.
    #[test]
    fn declared_scores_order_the_candidates_as_chrome_downloads_them() {
        assert!(declared_score(&cand("32x32")) > declared_score(&cand("16x16")));
        assert!(declared_score(&cand("16x16")) > declared_score(&cand("48x48")));
        assert!(declared_score(&cand("192x192")) > declared_score(&cand("512x512")));
        assert!(declared_score(&cand("")) > declared_score(&cand("32x32")));
        assert_eq!(declared_score(&cand("any")), 1.0);
        // A 32 px image is not perfect, so an unsized candidate after it is still downloaded ...
        assert!(frames_score(&[(32, 32)]) < 1.0);
        // ... and a 16 px image stops at a declared 48.
        assert!(declared_score(&cand("48x48")) <= frames_score(&[(16, 16)]));
    }

    #[test]
    fn a_png_header_is_read_through_lossy_text() {
        let mut png = vec![0x89u8, b'P', b'N', b'G', 13, 10, 26, 10, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&32u32.to_be_bytes());
        png.extend_from_slice(&32u32.to_be_bytes());
        let text = String::from_utf8_lossy(&png).into_owned();
        assert_eq!(image_sizes(&text, "image/png"), Some(vec![(32, 32)]));
    }

    #[test]
    fn the_manifest_icon_rules() {
        let doc = "http://h/p.html";
        let m = |j: &str| manifest_icon(&serde_json::from_str(j).unwrap(), "http://h/m.json", doc);
        let s = r#""name":"t","start_url":".","display":"standalone""#;
        assert_eq!(m(&format!(r#"{{{s},"icons":[{{"src":"a.png","sizes":"192x192"}},{{"src":"b.png","sizes":"512x512"}}]}}"#)).as_deref(), Some("http://h/a.png"));
        assert_eq!(m(&format!(r#"{{{s},"icons":[{{"src":"a.png","sizes":"144x144"}},{{"src":"b.png","sizes":"512x512"}}]}}"#)).as_deref(), Some("http://h/a.png"));
        assert_eq!(m(&format!(r#"{{{s},"icons":[{{"src":"a.png","sizes":"100x100"}},{{"src":"b.png","sizes":"143x143"}}]}}"#)), None);
        assert_eq!(m(r#"{"name":"t","display":"standalone","icons":[{"src":"a.png","sizes":"192x192"}]}"#), None);
        assert_eq!(m(&format!(r#"{{{s},"icons":[{{"src":"a.png","sizes":"192x192","purpose":"maskable"}}]}}"#)), None);
    }
}
