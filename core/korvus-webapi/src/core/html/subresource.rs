//! `core/html/subresource.rs` -- ELEMENT-INDUCED subresource loads (was
//! `dom/09j-subresource.js`).
//!
//! A browser does not only fetch what the parser found. Setting `img.src`, appending a `<script src>`,
//! or inserting a `<link rel=icon href>` each starts a real HTTP request, and each request carries a
//! DIFFERENT `Sec-Fetch-Dest` -- image, script, image again -- computed from the element that induced
//! it. Without this the engine issued none of them: `load_page` fetched the `<script src>` elements
//! present in the parsed HTML and nothing else, so an element created after load never reached the
//! network and its `load`/`error` event never fired.
//!
//! Two consequences, both measured. On the wire it means an entire class of request destinations was
//! missing from the traffic this engine produces. In WPT it means every `fetch/metadata` test built on
//! `induceRequest()` -- create an element, point it at `record-headers.py`, wait for `onload`, then ask
//! the server what arrived -- hung on a promise that could never settle, and the whole file timed out
//! with no verdict at all rather than with a failure.
//!
//! WHERE THIS IS DRIVEN FROM. Three seams, matching the three ways a browser starts one of these
//! loads -- all three converge on [`maybe_load`], which is the ONE place a subresource load is
//! issued:
//!   * an attribute change, through the native choke point every attribute write funnels into
//!     ([`crate::core::dom::shared::set_attribute_notify`], which already calls out to the
//!     MutationObserver registry from the same place) -- this is what makes a DETACHED `img` load,
//!     which is what most of the generated tests actually do;
//!   * an insertion, through `notify_childlist_sib` -- this is what makes a `<script src>` or a
//!     `<link>` load, since those only fetch once they are in a document;
//!   * THE PARSER, through [`start_parsed_subresources`] -- a document-order sweep of
//!     the finished tree, because the parser writes into the arena through neither of the other two
//!     and so produced, until that unit, no request at all for anything written in the markup.
//!
//! It is NOT a rendering pipeline. The bytes are fetched and, for a classic script, executed; an image
//! or a video is fetched and its `load` event fired without being decoded, because what a subresource
//! load is being asked to produce here is the REQUEST.
//!
//! WHERE THE BYTES COME FROM. Not from here: `korvus-webapi` owns no HTTP client. The load goes out
//! through [`spec::SubresourceFetch`], the kernel service `korvus-runtime` registers on the
//! `spec::kernel::Context` -- the kernel owns the page location a relative URL resolves against, the
//! worn identity's headers and the transport, and this file owns the one thing the kernel cannot know:
//! which element, through which attribute, induces a request with which destination and mode.
//!
//! WHY THE COMPLETION IS A NATIVE CONTINUATION. `op_subresource_fetch` returns a promise and the two
//! continuations below are Rust `v8::Function`s built by `build_fn_data` -- the same mechanism
//! `xml_http_request.rs` uses for `op_fetch` and `browsing_context.rs` uses for a navigation. There is
//! no JS body, no bridge slot and no page-visible callback anywhere on the path; V8's promise
//! machinery is what queues the completion, at exactly the point in the event loop the deleted JS
//! `.then` was queued at.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use deno_core::{op2, v8, OpState};

use crate::binding::protos::{build_fn_data, get_prop, str_val, unwrap_id, wrap_cached};
use crate::binding::HostDom;
use crate::core::html::browsing_context::{delay_load, fire_at, frame_navigated, new_load_delay_token, undelay_load};
use crate::core::html::html_script_element::{current_script, script_response_ok, set_current_script, ScriptFlavor};
use crate::core::html::isolation_policy::{header_value, headers_from_js};
use crate::core::xmlhttprequest::xml_http_request::deno_op;

// ---- the tables -------------------------------------------------------------------------------

/// (tag, attribute) -> the Fetch request destination the load takes. Keyed by lowercase tag name; the
/// value maps the source attribute to a destination. A tag that is not here does not load; an
/// attribute that is not in its row does not load from that element, which is the common case.
/// A new attribute here must pass [`attr_name_may_load`] too, or the attribute seam never sees its writes
/// (`tests::attr_names_cover_every_route` fails until it does).
fn sources(tag: &str) -> Option<&'static [(&'static str, &'static str)]> {
    Some(match tag {
        "img" => &[("src", "image"), ("srcset", "image")],
        "source" => &[("src", "image"), ("srcset", "image")],
        "input" => &[("src", "image")], // only for type=image; checked in `destination_for`
        "video" => &[("src", "video"), ("poster", "image")],
        "audio" => &[("src", "audio")],
        "track" => &[("src", "track")],
        "embed" => &[("src", "embed")],
        "iframe" => &[("src", "iframe")],
        "frame" => &[("src", "frame")],
        "object" => &[("data", "object")],
        "script" => &[("src", "script")],
        "link" => &[("href", "")], // destination depends on `rel`; resolved in `destination_for`
        "image" => &[("href", "image"), ("xlink:href", "image")], // SVG <image>
        "use" => &[("href", ""), ("xlink:href", "")],
        _ => return None,
    })
}

/// `<link rel=...>` -> destination. Chrome's `Sec-Fetch-Dest` for a stylesheet link is `style` and for
/// a prefetch it is the EMPTY destination (which serialises `empty`). An ICON is not here: the page
/// does not load it, the browser's favicon loader does, after `load` (`core/html/link_icons.rs`).
/// A rel this table does not know does not load at all, which is also what a browser does -- measured
/// on `<link rel=author>`: Chrome 150.0.7871.46 headless requests nothing for it.
fn link_rel_destination(rel: &str) -> Option<&'static str> {
    Some(match rel {
        "stylesheet" => "style",
        "prefetch" | "prerender" => "",
        "manifest" => "manifest",
        _ => return None,
    })
}

/// `<link rel=preload as=...>` -> destination. A preload's WHOLE POINT is that it carries the
/// destination of the load it is standing in for, which is why its `as` maps straight onto Fetch's
/// destination table rather than through [`link_rel_destination`].
///
/// AN `as` THAT IS ABSENT OR UNKNOWN MEANS NO LOAD AT ALL. Measured, because it is the opposite of
/// what "a preload is a hint, so fetch it anyway" would predict: `<link rel=preload href=x>` with no
/// `as` makes Chrome 150.0.7871.46 request NOTHING (0 ms, one request on the wire -- the page), while
/// `as=style` and `as=image` are both requested. HTML says the same: a preload with no valid `as` has
/// no destination, and a link whose destination cannot be determined is not fetched.
fn preload_as_destination(as_: &str) -> Option<&'static str> {
    Some(match as_ {
        "style" => "style",
        "image" => "image",
        "script" => "script",
        "font" => "font",
        "track" => "track",
        "audio" => "audio",
        "video" => "video",
        "document" => "document",
        "embed" => "embed",
        "object" => "object",
        "worker" => "worker",
        "fetch" => "",
        _ => return None,
    })
}

/// The image media types a `<picture>`'s `<source type=...>` is CONSIDERED for. A `<source>` naming a
/// type outside this set is skipped and the next candidate (ultimately the `<img>`) is taken, which
/// is exactly what a browser does with a format it cannot decode.
///
/// Measured: `<picture><source srcset=a.xyz type=image/xyz><img src=b.png></picture>` makes Chrome
/// request `b.png` and never `a.xyz`. Without this test the first `<source>` would win and the page
/// would fetch a file no browser would have asked for.
fn decodable_image_type(ty: &str) -> bool {
    matches!(
        ty.trim().to_ascii_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp" | "image/avif" | "image/svg+xml" | "image/bmp" | "image/x-icon" | "image/vnd.microsoft.icon" | "image/apng"
    )
}

/// Elements whose load only starts once they are IN a document. Everything else (img, video, audio,
/// ...) starts its load the moment its source attribute is set, attached or not -- which is exactly
/// what `element-img.sub.html` relies on: it never appends the image it creates.
fn needs_connection(tag: &str) -> bool {
    matches!(tag, "script" | "link" | "iframe" | "frame" | "object" | "embed")
}

/// The two tags whose response IS a nested browsing context's document.
fn frameish(tag: &str) -> bool {
    matches!(tag, "iframe" | "frame")
}

/// Where a frame whose navigation failed on the network commits its error page: Chromium's
/// `kUnreachableWebDataURL` (`content/public/common/url_constants.cc`), an opaque origin to every page.
const UNREACHABLE_WEB_DATA_URL: &str = "chrome-error://chromewebdata/";

/// Does this induced load DELAY THE DOCUMENT'S LOAD EVENT (`core/html/browsing_context.rs`)?
///
/// MEASURED, not reasoned about. Chrome 150.0.7871.46 headless over a loopback server that sleeps
/// 600 ms per resource, on a page that timestamps its own `load`: a page with nothing to wait for
/// fires `load` at 1 ms, so a 600 ms answer IS the delay. Per resource, each one dynamically
/// injected from script unless noted:
///
/// | resource | `load` fires at | delays? |
/// |---|---|---|
/// | `<script src>` parser-inserted, which also injects a second one | 1245 ms | yes, both |
/// | `<script async src>` (parser-inserted) | 642 ms | **yes** |
/// | `<script defer src>` (parser-inserted) | 643 ms | yes |
/// | injected `<script src>` (force-async) | 645 ms | yes |
/// | injected `<script src>` with `async = false` | 645 ms | yes |
/// | `<img src>` | 643 ms | yes |
/// | `<link rel=stylesheet>` | 646 ms | yes |
/// | `<object data>` | 643 ms | yes |
/// | `<embed src>` | 646 ms | yes |
/// | SVG `<image href>` | 643 ms | yes |
/// | `<link rel=prefetch>` | 1 ms | **no** |
/// | `<link rel=icon>` | 1 ms | **no** |
/// | `<link rel=manifest>` | 1 ms | **no** |
///
/// `<script async>` is the row worth naming, because the folklore is the opposite: HTML gives every
/// script element a "delaying the load event" boolean that is set for the whole of its fetch
/// regardless of `async`, and Chrome implements that. `async` is about the PARSER, not about `load`.
///
/// MEDIA IS THE ONE PLACE THIS ENGINE DELIBERATELY DOES NOT COPY CHROME. A media element delays the
/// load event only until it has HAVE_CURRENT_DATA -- a few frames, a few tens of kilobytes of a file
/// that may be hundreds of megabytes. There is no media pipeline here: the fetch below downloads the
/// WHOLE body before it can release anything, so a token on `<video>`/`<audio>` would hold the
/// document far LONGER than Chrome does, which is the one failure this change must not cause. They
/// take no token. Cost if wrong: a `load` handler that reads `video.readyState` on a dynamically
/// inserted media element sees 0 where Chrome sees >= 2.
/// A `<link>`'s own row is decided in [`destination_for`], which is the only place that knows the
/// `rel`: a stylesheet delays, and every other `<link>` (icon, prefetch, prerender, manifest, and --
/// measured -- EVERY `rel=preload`, whatever its `as` says) does not.
fn delays_load(tag: &str) -> bool {
    !matches!(tag, "video" | "audio" | "track" | "source")
}

/// Case-insensitive scheme test, without allocating a lowercase copy of the whole URL.
fn has_scheme(url: &str, scheme: &str) -> bool {
    url.len() > scheme.len()
        && url.is_char_boundary(scheme.len())
        && url[..scheme.len()].eq_ignore_ascii_case(scheme)
        && url.as_bytes()[scheme.len()] == b':'
}

// ---- per-element de-duplication ---------------------------------------------------------------

/// A load is started once per (element, resolved URL). Without this, setting `src` and then appending
/// the element would issue the request twice, and a server that records the request -- which is
/// precisely what these tests use -- would see the duplicate.
///
/// The JS kept a `WeakMap<Element, Set<string>>`; the arena node id is the same key without the
/// wrapper, and the map lives in `OpState` so it shares the isolate's lifetime.
#[derive(Default)]
pub struct StartedLoads(RefCell<HashMap<u32, HashSet<String>>>);

/// A fresh set for the next navigation, on the same terms as [`crate::reset_uncaught_errors`] and
/// [`crate::reset_unhandled_rejections`] — and for a sharper reason than tidiness.
///
/// THE KEY IS AN ARENA NODE ID, AND A NEW DOCUMENT REUSES THEM. `StartedLoads` lives in `OpState`,
/// which shares the ISOLATE's lifetime, while the DOM arena is rebuilt per navigation and allocates
/// its ids from the bottom again. So the second document's `<link>` got the first document's node
/// id, `claim` found the (id, url) pair already recorded, and the load was refused as a duplicate.
///
/// Measured on `tools/parity/fixtures/site1` over one `korvus serve` connection, before this
/// existed — three navigations to the same page:
///
/// ```text
/// navigation 1: / style.css script.js preload.js favicon.ico img.png frame.html missing.png lazy.png data.json
/// navigation 2: / script.js data.json
/// navigation 3: / script.js data.json
/// ```
///
/// Every element-induced load — stylesheet, images, iframe, preload, favicon — was silently dropped
/// from the second navigation onward, while the DOM still reported 18 nodes and the page looked
/// fine. `<script src>` survived only because `Engine::load_page` fetches those itself on a
/// different path. Nothing in the engine could see this before the request ledger existed, which is
/// exactly what the ledger is for.
pub fn reset_started_loads(op_state: &std::rc::Rc<RefCell<OpState>>) {
    let mut st = op_state.borrow_mut();
    if let Some(r) = st.try_borrow_mut::<DocumentRequests>() {
        r.0.clear();
    }
    // keyed by element id too: a refused frame's pending fallback entry dies with its document
    crate::core::timing::resource_timing::reset_frame_fallbacks(&mut st);
    if let Some(o) = st.try_borrow_mut::<InOrderScripts>() {
        *o = InOrderScripts::default();
    }
    if let Some(s) = st.try_borrow_mut::<StartedLoads>() {
        s.0.borrow_mut().clear();
    }
    if let Some(o) = st.try_borrow_mut::<ObjectOutcomes>() {
        o.0.clear();
    }
    if let Some(s) = st.try_borrow_mut::<LinkSheetFetches>() {
        *s = LinkSheetFetches::default();
    }
    if let Some(p) = st.try_borrow_mut::<PendingLinkReplays>() {
        p.0.clear();
    }
    if let Some(p) = st.try_borrow_mut::<PendingStyleLoads>() {
        p.0.clear();
    }
    crate::core::css::style_sheet::reset_minted_sheets(&mut st);
    // The image documents of the last page went with it; the new page's is the one armed for it, if any.
    if let Some(c) = st.try_borrow_mut::<ImageDocumentContent>() {
        c.open.clear();
        if let Some(u) = c.next_page.take() {
            c.open.insert(None, u);
        }
    }
}

// ---- <object>: its data, or its fallback content -----------------------------------

/// How each `<object>`'s load ended, by node id: the URL that was loaded, and `Some(content type)` when
/// it answered 2xx or `None` when it failed. Keyed like [`StartedLoads`] and reset with it.
#[derive(Default)]
struct ObjectOutcomes(HashMap<u32, (String, Option<String>)>);

/// Does an `<object>` whose content is typed `ty` show that content rather than its fallback?
///
/// Measured on Chrome 150.0.7871.46 over loopback, `A<object ...>OBJ</object>B`, reading `innerText`
/// after `load` -- `AB` is "shows its data", `AOBJB` is "shows its fallback":
///
/// ```text
/// no data, no type        AOBJB      data=''                 AOBJB    data 404 / 500          AOBJB
/// type=image/png          AB         type=text/html          AB       type=application/x-foo  AOBJB
/// type=application/pdf    AB         data=x.png              AB       data=x.svg              AB
/// data=x.txt / .css / .js AB         data=x.json / .xml      AB       data=x.mp4              AB
/// data=x.bin (octet)      AOBJB      data=x.bin type=text/html AB     data=x.png type=application/x-foo AOBJB
/// type=application/x-shockwave-flash data=x.swf                       AOBJB
/// data=missing.png, read by a script before the load settled          AB
/// ```
///
/// So the `type` attribute, when present, decides over the response's `Content-Type`; a type that
/// names something the browser renders itself -- an image, a document (HTML, XML, text, JSON,
/// script), media, or a PDF when the persona has a PDF viewer -- shows the data, and anything else
/// is a plugin this browser does not have. The MIME families are WHATWG MIME Sniffing's own.
///
/// NOT MODELLED, and measured: `data=x.bin type=image/png` is `AOBJB` in Chrome -- the image fails to
/// DECODE -- and `AB` here, because a completion reads no image bytes (R7).
fn object_renders_type(ty: &str, pdf_viewer: bool) -> bool {
    let essence = ty.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let Some((top, sub)) = essence.split_once('/') else { return false };
    match top {
        "text" | "video" | "audio" => true,
        "image" => decodable_image_type(&essence),
        "application" => {
            matches!(sub, "xml" | "xhtml+xml" | "json" | "javascript" | "ecmascript" | "x-javascript")
                || sub.ends_with("+xml")
                || sub.ends_with("+json")
                || (sub == "pdf" && pdf_viewer)
        }
        _ => false,
    }
}

/// Is `<object>` `id` showing its fallback content (its children) rather than its data?
///
/// With a `data` URL and no settled load it is NOT: Chrome renders neither while the load is pending.
pub(crate) fn object_shows_fallback(scope: &mut v8::PinScope, id: u32) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (data, ty) = {
        let st = op_state.borrow();
        let Some(host) = st.try_borrow::<HostDom>() else { return true };
        let attr = |n: &str| host.0.get_attribute(id, n).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        (attr("data"), attr("type"))
    };
    let pdf_viewer = crate::core::navigator::navigator::persona(scope).is_some_and(|p| p.navigator_extra().pdf_viewer_enabled);
    let Some(data) = data else {
        return !ty.is_some_and(|t| object_renders_type(&t, pdf_viewer));
    };
    let home = home_of(scope, id);
    let url = resolve(&base_for(scope, home), &data);
    let outcome = {
        let st = op_state.borrow();
        st.try_borrow::<ObjectOutcomes>().and_then(|o| o.0.get(&id).filter(|(u, _)| *u == url).map(|(_, r)| r.clone()))
    };
    match outcome {
        None => false,
        Some(None) => true,
        // A response with no `Content-Type` at all is sniffed into a document, not refused (ruling).
        Some(Some(content_type)) => match ty.as_deref().unwrap_or(&content_type) {
            "" => false,
            t => !object_renders_type(t, pdf_viewer),
        },
    }
}

/// Record how an `<object>`'s load ended. `content_type` is `None` for a failed load.
fn note_object_outcome(scope: &mut v8::PinScope, id: u32, url: &str, content_type: Option<String>) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let is_object = st.try_borrow::<HostDom>().is_some_and(|h| h.0.is_html_ns(id) && h.0.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("object")));
    if !is_object {
        return;
    }
    if st.try_borrow::<ObjectOutcomes>().is_none() {
        st.put(ObjectOutcomes::default());
    }
    st.borrow_mut::<ObjectOutcomes>().0.insert(id, (url.to_string(), content_type));
}

/// `<link rel=stylesheet>` elements whose stylesheet fetches are in flight, and the ones whose FIRST
/// fetch has ended.
///
/// A linked sheet does not exist until its response does. Chrome 150, a stylesheet inserted from
/// script and answered 1.5 s later: `link.sheet` is `null` right after `appendChild` and still `null`
/// 500 ms in, non-null with its one rule inside the link's own `load` handler -- and non-null (empty)
/// inside `error` for a 404. This engine minted the sheet on first read, so it answered non-null from
/// the moment the element was connected. Only the FIRST fetch counts: a link whose `href` changes
/// keeps the sheet it has while the next one loads.
///
/// AND A LINK THAT CANNOT FETCH HAS NO SHEET AT ALL. Chrome 152.0.7977.82, 2026-10-23: a connected
/// `<link rel=stylesheet>` with no `href`, `href=""`, `href=" "` or `href="http://["` has `sheet` null and is not
/// in `document.styleSheets`, then and 800 ms later (as `<link rel=stylesheet disabled>` is not, which
/// `page::is_stylesheet_link` already says). This engine asked only "is a fetch in flight?", so each of those had an
/// empty sheet in the list from the moment it was connected: `document.styleSheets` counted `<style>` plus
/// `<link rel=stylesheet>` 2 where Chrome counts 1. `unloadable` holds the links whose URL did not parse ([`induced`]).
///
/// `sheet_url` is the URL the link's last ended fetch asked for -- its sheet's `href` -- and `failed` the links whose
/// last fetch failed, which is the event a re-insertion fires again ([`replay_link_load`]). `bodies` is each link's
/// last response as its sheet was parsed from it -- text, final URL, origin-clean -- which is what its next sheet holds
/// when the one it has is detached ([`crate::core::css::style_sheet::detach_element_sheet`]). `ended_disabled` are
/// the links whose last fetch ended while they were `disabled` ([`disabled_stylesheet_link`]). `wanted` is the URL a
/// link's `href` named when it last started a fetch or was written ([`link_fetch_current`]).
#[derive(Default)]
struct LinkSheetFetches {
    /// The URLs each link's stylesheet fetches in flight asked for, one entry a fetch.
    pending: HashMap<u32, Vec<String>>,
    ended: HashSet<u32>,
    unloadable: HashSet<u32>,
    sheet_url: HashMap<u32, String>,
    failed: HashSet<u32>,
    bodies: HashMap<u32, (std::rc::Rc<str>, Option<String>, bool)>,
    ended_disabled: HashSet<u32>,
    wanted: HashMap<u32, String>,
    /// The stylesheet URLs this document has had a response for that is fresh for a while ([`fresh_for_reuse`]): a
    /// sheet load of one again is Blink's memory cache's, which reports no second resource-timing entry ([`issue`]).
    fresh: HashSet<String>,
}

fn link_fetches(st: &mut OpState) -> &mut LinkSheetFetches {
    if st.try_borrow::<LinkSheetFetches>().is_none() {
        st.put(LinkSheetFetches::default());
    }
    st.borrow_mut::<LinkSheetFetches>()
}

/// Is a response with these headers fresh for a while -- a `max-age` above 0 and neither `no-store` nor `no-cache`? Such
/// a stylesheet is answered from Blink's memory cache when its document loads it again ([`LinkSheetFetches`]`::fresh`).
/// Explicit freshness only: a response fresh by heuristics alone still makes its entry.
fn fresh_for_reuse(headers: &[(String, String)]) -> bool {
    let mut fresh = false;
    for (k, v) in headers {
        if !k.eq_ignore_ascii_case("cache-control") {
            continue;
        }
        for d in v.split(',').map(str::trim) {
            if d.eq_ignore_ascii_case("no-store") || d.eq_ignore_ascii_case("no-cache") {
                return false;
            }
            if let Some(n) = d.get(..8).filter(|p| p.eq_ignore_ascii_case("max-age=")).and_then(|_| d[8..].trim_matches('"').parse::<u64>().ok()) {
                fresh = n > 0;
            }
        }
    }
    fresh
}

fn link_sheet_fetch_started(op_state: &std::rc::Rc<RefCell<OpState>>, id: u32, url: &str) {
    let mut st = op_state.borrow_mut();
    let f = link_fetches(&mut st);
    f.pending.entry(id).or_default().push(url.to_string());
    f.unloadable.remove(&id);
    f.wanted.insert(id, url.to_string());
}

/// Whether `id`'s stylesheet URL fails to parse, so that it loads nothing and has no sheet ([`LinkSheetFetches`]).
fn link_sheet_unloadable(op_state: &std::rc::Rc<RefCell<OpState>>, id: u32, unloadable: bool) {
    let mut st = op_state.borrow_mut();
    if unloadable {
        link_fetches(&mut st).unloadable.insert(id);
    } else if let Some(f) = st.try_borrow_mut::<LinkSheetFetches>() {
        f.unloadable.remove(&id);
    }
}

/// One fetch fewer in flight for `id`: the one for `url`, else (a parser fetch whose `href` has moved since) the oldest.
fn link_fetch_settled(f: &mut LinkSheetFetches, id: u32, url: &str) -> bool {
    let Some(urls) = f.pending.get_mut(&id) else { return false };
    let at = urls.iter().position(|u| u == url).unwrap_or(0);
    if at < urls.len() {
        urls.remove(at);
    }
    if urls.is_empty() {
        f.pending.remove(&id);
    }
    true
}

/// A stylesheet fetch of `url` for `id` has ended. True when an earlier one had: the sheet the link has is replaced
/// by this one's, and is detached ([`crate::core::css::style_sheet::detach_element_sheet`]).
fn link_sheet_fetch_ended(op_state: &std::rc::Rc<RefCell<OpState>>, id: u32, url: &str) -> bool {
    let mut st = op_state.borrow_mut();
    let Some(f) = st.try_borrow_mut::<LinkSheetFetches>() else { return false };
    if !link_fetch_settled(f, id, url) {
        return false;
    }
    f.sheet_url.insert(id, url.to_string());
    !f.ended.insert(id)
}

/// How the last stylesheet fetch for `id` ended ([`LinkSheetFetches`]`::failed`); a failed one leaves no response
/// for a next sheet to hold.
fn link_sheet_outcome(op_state: &std::rc::Rc<RefCell<OpState>>, id: u32, ok: bool) {
    let mut st = op_state.borrow_mut();
    let Some(f) = st.try_borrow_mut::<LinkSheetFetches>() else { return };
    if ok {
        f.failed.remove(&id);
    } else {
        f.failed.insert(id);
        f.bodies.remove(&id);
    }
}

/// `id`'s last stylesheet response as its sheet was parsed from it: text, final URL, origin-clean ([`LinkSheetFetches`]).
pub(crate) fn link_sheet_body(st: &OpState, id: u32) -> Option<(std::rc::Rc<str>, Option<String>, bool)> {
    st.try_borrow::<LinkSheetFetches>().and_then(|f| f.bodies.get(&id).cloned())
}

/// The URL `id`'s current sheet was fetched from: its `href` ([`LinkSheetFetches`]).
pub(crate) fn link_sheet_url(scope: &mut v8::PinScope, id: u32) -> Option<String> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<LinkSheetFetches>().and_then(|f| f.sheet_url.get(&id).cloned())
}

/// Has `id`'s first stylesheet fetch ended, with no other in flight? Then its sheet is the one it has.
fn link_sheet_settled(op_state: &std::rc::Rc<RefCell<OpState>>, id: u32) -> bool {
    let st = op_state.borrow();
    st.try_borrow::<LinkSheetFetches>().is_some_and(|f| f.ended.contains(&id) && !f.pending.contains_key(&id))
}

/// The `<style>` elements whose `load`/`error` is queued, each with the load-delay token it holds ([`queue_style_load`]).
#[derive(Default)]
struct PendingStyleLoads(HashMap<u32, u32>);

/// Does a `<style>`'s sheet, and so its `load`, exist? An HTML `<style>` in a document whose `type` is absent, empty or
/// `text/css` (ASCII case-insensitively, nothing trimmed): Chrome 152 gives `type=" TEXT/CSS "` and `type="text/css;
/// charset=utf-8"` no sheet, an SVG `<style>` no event.
fn style_load_applies(scope: &mut v8::PinScope, id: u32) -> bool {
    let ok = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().is_some_and(|h| {
            let dom = &h.0;
            dom.is_html_ns(id) && dom.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("style")) && dom.get_attribute(id, "type").is_none_or(|t| t.is_empty() || t.eq_ignore_ascii_case("text/css"))
        })
    };
    ok && element_connected(scope, id)
}

/// A `<style>` was inserted into a document, or its text changed there: its sheet is made again, and once that
/// sheet's `@import`s have settled it fires `load` -- `error` when one failed -- as a task that delays the document's
/// `load`. Chrome 152.0.7977.82, 2026-10-23:
///
/// ```text
///   appended by script                  `load` after the script's microtasks, before a setTimeout(0)
///   an empty one, media=print           `load`
///   text replaced (textContent =)       `load` (twice: the old text's removal and the new text's insertion)
///   removed and appended again          `load` again;  an insertRule: nothing
///   @import of a 200 / of a 404         `load` once it arrived / `error`
///   in the markup                       `load` (`error` for a 404 @import), before the window's `load`
///   in a detached <div>, type=text/foo, an SVG <style>     nothing
/// ```
///
/// This engine fired nothing at a `<style>`, ever. Not modelled: a removal of the text alone, a `data` write to it,
/// and the first of the two events of a `textContent` write -- the removal half has no seam here -- and a PARSED
/// `<style>`'s event, which Chrome fires before the parser runs the next script: a task queued at the parsed-element
/// sweep would fire after every parser script and `DOMContentLoaded`, where a listener a script added after the
/// style would hear what Chrome's never does.
pub(crate) fn queue_style_load(scope: &mut v8::PinScope, id: u32) {
    if !style_load_applies(scope, id) {
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if op_state.borrow().try_borrow::<PendingStyleLoads>().is_some_and(|p| p.0.contains_key(&id)) {
        return;
    }
    let token = new_load_delay_token(scope);
    delay_load(scope, token);
    {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<PendingStyleLoads>().is_none() {
            st.put(PendingStyleLoads::default());
        }
        st.borrow_mut::<PendingStyleLoads>().0.insert(id, token);
    }
    crate::core::intersection_observer::rendering::queue_task_with_id(scope, cb_style_load, id as u64, 0.0);
}

fn cb_style_load(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = crate::binding::protos::body_data(scope, &args).number_value(scope).map(|n| n as u32) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let token = op_state.borrow_mut().try_borrow_mut::<PendingStyleLoads>().and_then(|p| p.0.remove(&id));
    let Some(token) = token else { return };
    if !sheet_load_settled(scope, id, token) {
        // Its imports are requested now rather than at the next style frame, each with its own fetch's end to
        // settle it -- so the wait, and the document's `load` it holds, always ends.
        crate::core::css::sheet_loads::wait_for_imports(scope, id, token);
        crate::core::css::sheet_loads::request_imports(scope);
    }
}

/// Fire `<style>` or stylesheet `<link>` `id`'s `load` (or `error`) if its sheet's imports have settled, releasing
/// `token`: true when done with it -- fired, or no longer a sheet in a document -- false while an import is on its way.
/// A link waits only once its own response was a sheet ([`cb_load_ok`]), so `error` here is an import's.
pub(crate) fn sheet_load_settled(scope: &mut v8::PinScope, id: u32, token: u32) -> bool {
    let link = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().map(|h| page::is_stylesheet_link(&h.0, id))
    };
    let applies = match link {
        Some(true) => element_connected(scope, id),
        _ => style_load_applies(scope, id),
    };
    if !applies {
        undelay_load(scope, token);
        return true;
    }
    let handle = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        crate::core::css::cssom::sheet_for_element(&mut st, id)
    };
    let state = crate::core::css::sheet_loads::import_state(scope, handle);
    if state == crate::core::css::sheet_loads::ImportState::Pending {
        return false;
    }
    fire(scope, id, if state == crate::core::css::sheet_loads::ImportState::Failed { "error" } else { "load" });
    undelay_load(scope, token);
    true
}

/// The links whose replayed `load`/`error` is queued ([`replay_link_load`]).
#[derive(Default)]
struct PendingLinkReplays(HashSet<u32>);

/// A stylesheet link whose sheet was loaded is inserted again, or enabled again: Chrome processes it afresh and
/// fires its `load` (`error` for a failed one) again. Chrome 152.0.7977.82, 2026-10-23: a loaded link removed and
/// appended again, moved from `<head>` to `<body>` in one step, or `disabled` and enabled again fires `load` each
/// time -- from its memory cache for a cacheable sheet (no request, the new sheet there at once), from a second
/// request for a `no-store` one. This engine fired nothing, so a `link.onload` a page waits on after moving a link
/// never came. Here the response the link has is its sheet again, as a cacheable one is, and the event is a task.
fn replay_link_load(scope: &mut v8::PinScope, id: u32) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<PendingLinkReplays>().is_none() {
            st.put(PendingLinkReplays::default());
        }
        if !st.borrow_mut::<PendingLinkReplays>().0.insert(id) {
            return;
        }
    }
    crate::core::intersection_observer::rendering::queue_task_with_id(scope, cb_link_replay, id as u64, 0.0);
}

fn cb_link_replay(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = crate::binding::protos::body_data(scope, &args).number_value(scope).map(|n| n as u32) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let queued = op_state.borrow_mut().try_borrow_mut::<PendingLinkReplays>().is_some_and(|p| p.0.remove(&id));
    let sheet = op_state.borrow().try_borrow::<HostDom>().is_some_and(|h| page::is_stylesheet_link(&h.0, id));
    if !queued || !sheet || !link_sheet_settled(&op_state, id) || !element_connected(scope, id) {
        return;
    }
    let failed = op_state.borrow().try_borrow::<LinkSheetFetches>().is_some_and(|f| f.failed.contains(&id));
    fire(scope, id, if failed { "error" } else { "load" });
}

/// May this stylesheet `<link>` have a `sheet` ([`LinkSheetFetches`])? Not while its first fetch is on its way, not
/// when its `href` can fetch nothing -- none, a blank one, one that does not parse -- whatever it fetched before, and
/// not when its last fetch ended while it was `disabled`. Chrome 152.0.7977.82, 2026-10-23, a loaded link whose
/// `href` is then removed, set to `""` or to `"http://["`: `sheet` null at once and after, out of
/// `document.styleSheets`, unstyled, the old sheet's `ownerNode` null and no event; this engine kept the loaded sheet
/// listed and styling. In the borrow the caller holds.
pub(crate) fn link_sheet_may_exist_in(st: &OpState, dom: &korvus_dom::Dom, id: u32) -> bool {
    if dom.get_attribute(id, "href").is_none_or(|h| h.trim().is_empty()) {
        return false;
    }
    let Some(f) = st.try_borrow::<LinkSheetFetches>() else { return true };
    if f.unloadable.contains(&id) || f.ended_disabled.contains(&id) {
        return false;
    }
    f.ended.contains(&id) || !f.pending.contains_key(&id)
}

/// The URL stylesheet link `id` names as far as its fetches go ([`LinkSheetFetches`]`::wanted`), if it has fetched.
fn link_wanted(scope: &mut v8::PinScope, id: u32) -> Option<String> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<LinkSheetFetches>().and_then(|f| f.wanted.get(&id).cloned())
}

/// A stylesheet link's `href` was written or removed; it named `before` ([`link_wanted`]) until then. One that now
/// names nothing it can fetch has no sheet from now on ([`link_sheet_may_exist_in`]). The sheet it had is detached,
/// keeping its rules, and the cascade's row is emptied; its next fetch is a first one again.
///
/// The URL it moved away from is no longer started for it ([`unclaim_sheet`]) -- unless its fetch is still in flight,
/// which a write back joins, and whose end forgets it when it is not ([`link_fetch_cancelled`]). Chrome
/// 152.0.7977.82, 2026-10-23: a loaded link whose `href` is cleared (`""` or removed) and then written back, a toggle
/// A -> B -> A, and `href` set to B and back to A in one task all fetch A again and fire `load` -- the network asked
/// again for a `no-store` A, its memory cache answering a cacheable one -- and a link whose A is still in flight when
/// it comes back to it asks for A once and fires one `load`. With the URL kept as started, writing it back fetched
/// nothing and fired nothing: the cleared link kept an empty sheet, unstyled, and the toggle stayed on B.
fn link_href_changed(scope: &mut v8::PinScope, id: u32, before: Option<String>) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // Only a link that fetched a sheet has one to lose or a fetch to cancel: every other `href` write (an `<a>`'s)
    // stops at this lookup.
    if before.is_none() && !op_state.borrow().try_borrow::<LinkSheetFetches>().is_some_and(|f| f.wanted.contains_key(&id)) {
        return;
    }
    // What it names now ([`link_fetch_current`]): nothing, when it can fetch nothing.
    let raw = op_state.borrow().try_borrow::<HostDom>().and_then(|h| h.0.get_attribute(id, "href").map(|v| v.trim().to_string())).filter(|r| !r.is_empty());
    let wanted = raw.map(|r| resolve(&node_base_url(scope, id), &r)).filter(|u| chrome_parses(u));
    let gone = {
        let mut st = op_state.borrow_mut();
        let f = link_fetches(&mut st);
        let left = before.filter(|b| wanted.as_deref() != Some(b.as_str()) && !f.pending.get(&id).is_some_and(|p| p.contains(b)));
        match wanted {
            Some(u) => f.wanted.insert(id, u),
            None => f.wanted.remove(&id),
        };
        let ended = f.ended.contains(&id);
        if let Some(b) = left {
            unclaim_sheet(&st, id, &b);
        }
        let st = &*st;
        let Some(h) = st.try_borrow::<HostDom>() else { return };
        ended && page::link_rel_has(&h.0, id, "stylesheet") && !link_sheet_may_exist_in(st, &h.0, id)
    };
    if !gone {
        return;
    }
    crate::core::css::style_sheet::detach_element_sheet(scope, id, crate::core::css::style_sheet::Fresh::Empty);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if let Some(f) = st.try_borrow_mut::<LinkSheetFetches>() {
        f.ended.remove(&id);
        f.sheet_url.remove(&id);
        f.failed.remove(&id);
        f.bodies.remove(&id);
    }
}

/// Is `id` a stylesheet `<link>` carrying a `disabled` attribute? Such a link is not loaded by the element, and
/// one the PARSER met is fetched all the same but heard from by nobody. Chrome 152.0.7977.82, 2026-10-23,
/// `no-store` loopback server:
///
/// ```text
///   script-inserted <link rel=stylesheet disabled>     no request, no load/error, sheet null
///   the same, then link.disabled = false               a request, then load; the sheet has its 2 rules
///   <link rel=stylesheet disabled> in the markup,      requested, `link` resource entry, the window's load waits
///   its response 1.2 s away                              for it; no load/error at the element; sheet null
///   the same, link.disabled = false 1.5 s after load   sheet null, then a SECOND request and entry, then load;
///                                                        2 rules
/// ```
///
/// This engine loaded either at insertion and fired `load` at it, and then -- the bytes having arrived while the
/// link was no stylesheet (`page::is_stylesheet_link`), so dropped -- `link.disabled = false` showed a sheet with 0
/// rules that never filled: a theme toggle's sheet never applied. Now a script's is not loaded ([`maybe_load_for`]),
/// a parsed one's load completes silently and forgets its claim ([`silent_sheet_end`]), and taking the attribute
/// away loads either afresh ([`resource_attr_removed`]).
///
/// A link disabled AFTER its fetch started is another case (Chrome 152, same server, `appendChild(link);
/// link.disabled = true`, the sheet 200 ms away): its `load` fires when the sheet arrives, and enabling it later
/// requests nothing, fires nothing and leaves it no sheet (`sheet` null, not in `document.styleSheets`, unstyled):
/// `ended_disabled` in [`LinkSheetFetches`]. (Chrome shows that sheet, rules readable, while the link is still
/// disabled; this engine has no sheet for a disabled link, `page::is_stylesheet_link`.) A link loaded and THEN
/// disabled and enabled again has a new sheet and fires `load` again ([`replay_link_load`]) without a request -- as
/// Chrome's memory cache answers a cacheable sheet; a `no-store` one Chrome asks the network for again, and this
/// engine has no HTTP cache to tell the two apart with.
fn disabled_stylesheet_link(dom: &korvus_dom::Dom, id: u32) -> bool {
    dom.get_attribute(id, "disabled").is_some() && page::link_rel_has(dom, id, "stylesheet")
}

/// Stylesheet link `id`'s fetch of `url` has ended (not unheard, not cancelled): one that is not its first replaces
/// the sheet it has -- detached, and the cascade's row emptied for this response to fill, or to stay empty when it
/// failed: Chrome 152.0.7977.82, 2026-10-23, a loaded link's `href` set to a 404 has a new sheet with 0 rules and
/// styles nothing, where this engine kept the old rules in the new sheet and in the cascade. `delays` is a sheet load
/// (not a preload): one that ends while the link is `disabled` leaves it no sheet ([`disabled_stylesheet_link`]).
fn link_sheet_ended(scope: &mut v8::PinScope, id: u32, url: &str, delays: bool) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if link_sheet_fetch_ended(&op_state, id, url) {
        crate::core::css::style_sheet::detach_element_sheet(scope, id, crate::core::css::style_sheet::Fresh::Empty);
    }
    if !delays {
        return;
    }
    let mut st = op_state.borrow_mut();
    let disabled = st.try_borrow::<HostDom>().is_some_and(|h| disabled_stylesheet_link(&h.0, id));
    let f = link_fetches(&mut st);
    if disabled {
        f.ended_disabled.insert(id);
    } else {
        f.ended_disabled.remove(&id);
    }
}

/// A stylesheet load of `url` for `id` that the PARSER started while the link was `disabled` ([`issue`]'s `silent`)
/// has ended, and the link is disabled still: true when it has, after undoing what the load recorded -- no first fetch
/// has ended for the link's `sheet`, and the URL is unclaimed so that enabling the link loads it again. The caller
/// fires nothing at the element and still releases the load's delay token.
///
/// ONLY such a load. The first cut asked "is the link disabled now?" alone, so a script's link disabled while its
/// sheet was on its way ended unheard as well: Chrome 152.0.7977.82, 2026-10-23, `appendChild(link); link.disabled =
/// true`, its sheet 200 ms away: `load` fires (a `link.onload` promise resolves) and enabling it later requests
/// nothing and fires nothing; that build fired nothing, then requested the sheet again and fired `load` on enabling.
fn silent_sheet_end(scope: &mut v8::PinScope, id: u32, url: &str) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if !st.try_borrow::<HostDom>().is_some_and(|h| disabled_stylesheet_link(&h.0, id)) {
        return false;
    }
    if let Some(f) = st.try_borrow_mut::<LinkSheetFetches>() {
        link_fetch_settled(f, id, url);
    }
    unclaim_sheet(&st, id, url);
    true
}

/// Forget that `id` started a stylesheet load of `url`, so that the link can start it again.
fn unclaim_sheet(st: &OpState, id: u32, url: &str) {
    if let Some(s) = st.try_borrow::<StartedLoads>() {
        if let Some(set) = s.0.borrow_mut().get_mut(&id) {
            set.remove(&claim_key("link", "style", true, url));
        }
    }
}

/// Is `url` still what stylesheet link `id` names? A fetch for an `href` the link no longer has was CANCELLED when
/// the attribute moved on: Chrome 152.0.7977.82, 2026-10-23, `href` set to a sheet 200 ms away and then, in the same
/// task, to one 50 ms away: one `load`, with the second sheet's 2 rules and URL as its `href`; both are requested. This
/// engine fired `load` twice and kept the sheet of whichever fetch ended last -- the stale one, 1 rule, its URL.
///
/// What it names is what it named when it was last written or started a fetch ([`LinkSheetFetches`]`::wanted`), not
/// its `href` resolved again now: a `history.pushState` moves the base a relative `href` resolves against, and moves
/// no fetch.
fn link_fetch_current(scope: &mut v8::PinScope, id: u32, url: &str) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<LinkSheetFetches>().is_none_or(|f| f.wanted.get(&id).is_some_and(|w| w == url))
}

/// A cancelled stylesheet fetch ([`link_fetch_current`]) has ended: it is no longer in flight, and its URL can be
/// started again.
fn link_fetch_cancelled(scope: &mut v8::PinScope, id: u32, url: &str) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if let Some(f) = st.try_borrow_mut::<LinkSheetFetches>() {
        link_fetch_settled(f, id, url);
    }
    unclaim_sheet(&st, id, url);
}

/// Does Chrome's URL parser take `url`? HTML's "if url is failure, then return" is GURL's failure in Chrome, and the
/// WHATWG parser (`url::Url::parse`) refuses two hosts GURL takes: a SPACE, which GURL escapes, and an ASCII `xn--`
/// label that is not valid punycode, which it keeps as written. Chrome 152.0.7977.82, 2026-10-23, one script-inserted
/// `<link rel=stylesheet>` per `href`:
///
/// ```text
///   requested (as http://a%20b/ ...), `error`, a sheet    http://a b/  http://a  b.c/  http://a%20b/  http://xn--/
///                                                         http://xn--a/  http://xn--zz--/
///   not requested, no event, no sheet                     http://[  http://%zz/  http://127.0.0.1:99999/  http://1.2.3.4.5/
///                                                         http://a<b/  http://a^b/  http://a|b/  http://a%00b/
///                                                         http://a\u05D0b1.com/  http://a\u200Db/  http://a\u007Fb/
/// ```
///
/// The first cut refused the first row as well: no request, no event, no sheet, where Chrome fires `error`.
fn chrome_parses(url: &str) -> bool {
    match url::Url::parse(url) {
        Ok(_) => true,
        Err(url::ParseError::IdnaError | url::ParseError::InvalidDomainCharacter) => {
            // The host: after `scheme://` and any userinfo, up to the path, query or fragment.
            let Some(at) = url.find("://").map(|i| i + 3) else { return false };
            let end = url[at..].find(['/', '?', '#', '\\']).map_or(url.len(), |e| at + e);
            let start = url[at..end].rfind('@').map_or(at, |a| at + a + 1);
            let host = url[start..end].replace("%20", "a").replace(' ', "a");
            let host = if host.is_ascii() {
                host.split('.').map(|l| if l.len() >= 4 && l[..4].eq_ignore_ascii_case("xn--") { "a" } else { l }).collect::<Vec<_>>().join(".")
            } else {
                host
            };
            url::Url::parse(&format!("{}{host}{}", &url[..start], &url[end..])).is_ok()
        }
        Err(_) => false,
    }
}

/// Is the element still in its document? A stylesheet link removed while its fetch was in flight
/// still delays the document's `load` -- Chrome 150, 1.5 s sheet, link removed right after insertion:
/// `load` at 1504 ms -- but fires nothing at the element.
/// Is `id` connected -- to the page's document or to a frame's: a frame document's stylesheet
/// `<link>` fired neither `load` nor `error`, since only the page's tree counted (Chrome 150, measured: a refused
/// sheet in a srcdoc frame's `<head>` fires `error`).
fn element_connected(scope: &mut v8::PinScope, id: u32) -> bool {
    connected_anywhere(scope, id)
}

/// Has `url` already been started for `id`? The read half of [`claim`], for a caller that must decide
/// something else (a lazy deferral) before it commits to the load.
fn claimed(op_state: &std::rc::Rc<RefCell<OpState>>, id: u32, url: &str) -> bool {
    let st = op_state.borrow();
    st.try_borrow::<StartedLoads>().is_some_and(|s| s.0.borrow().get(&id).is_some_and(|set| set.contains(url)))
}

/// Record `url` as started for `id`; `false` if it already was.
fn claim(op_state: &std::rc::Rc<RefCell<OpState>>, id: u32, url: &str) -> bool {
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<StartedLoads>().is_none() {
        st.put(StartedLoads::default());
    }
    let started = st.borrow::<StartedLoads>();
    let fresh = started.0.borrow_mut().entry(id).or_default().insert(url.to_string());
    fresh
}

// ---- the algorithm ----------------------------------------------------------------------------

/// `loading=lazy`, HTML's "lazy loading attribute": an enumerated attribute, matched ASCII
/// case-insensitively (measured: `loading=LAZY` defers exactly as `lazy` does; `eager` and an invalid
/// value load at once).
pub(crate) fn is_lazy(dom: &korvus_dom::Dom, id: u32) -> bool {
    dom.get_attribute(id, "loading").is_some_and(|v| v.eq_ignore_ascii_case("lazy"))
}

fn parent_is_picture(dom: &korvus_dom::Dom, id: u32) -> Option<u32> {
    let p = dom.parent(id)?;
    dom.tag_name(p).is_some_and(|t| t.eq_ignore_ascii_case("picture")).then_some(p)
}

/// The `<img>` a `<picture>` presents: its first `<img>` child. A `<source>` only ever loads THROUGH
/// that image -- it is a candidate list, not an element that fetches.
fn picture_img(dom: &korvus_dom::Dom, picture: u32) -> Option<u32> {
    dom.child_elements(picture).into_iter().find(|c| dom.tag_name(*c).is_some_and(|t| t.eq_ignore_ascii_case("img")))
}

/// The URL an `<img>` selects, unresolved -- `None` (or empty) means it requests nothing.
///
/// A `<picture>` IS A CHOICE, NOT A LIST (Blink `HTMLImageElement::FindBestFitImageFromPictureParent`):
/// its `<source>` children BEFORE the image are walked in order, a source with no `srcset`, a `type`
/// this browser cannot decode or a `media` that does not match is skipped, and the first source whose
/// `srcset` yields a candidate wins; the image's own `srcset`/`src` is the fallback. Measured on
/// Chrome 150.0.7871.46:
///
/// ```text
/// <source a><source b><img c>                               a only
/// <source a type=image/xyz><img c>                          c
/// <source media="(min-width: 800px)" wide><source n><img>   wide   (a 1920px window)
/// <source media="(max-width: 800px)" narrow><img i>         i
/// <source sizes=100px srcset="90w, 110w"><img>              110w
/// <source type=image/webp media="(prefers-color-scheme: dark)" d><img i>   i
/// ```
///
/// Candidate selection within one list is [`crate::core::html::srcset`].
fn image_source(dom: &korvus_dom::Dom, img: u32, ctx: &page::style::media::MediaContext) -> Option<String> {
    image_source_density(dom, img, ctx).map(|(u, _)| u)
}

/// [`image_source`], with the selected candidate's pixel density (`srcset::pick_density`; 1 for a plain `src`).
fn image_source_density(dom: &korvus_dom::Dom, img: u32, ctx: &page::style::media::MediaContext) -> Option<(String, f32)> {
    use crate::core::html::srcset;
    if let Some(picture) = parent_is_picture(dom, img) {
        for child in dom.child_elements(picture) {
            if child == img {
                break;
            }
            if !dom.tag_name(child).is_some_and(|t| t.eq_ignore_ascii_case("source")) {
                continue;
            }
            let Some(set) = dom.get_attribute(child, "srcset").filter(|s| !s.is_empty()) else { continue };
            if dom.get_attribute(child, "type").is_some_and(|t| !t.trim().is_empty() && !decodable_image_type(t)) {
                continue;
            }
            if let Some(m) = dom.get_attribute(child, "media").map(str::trim).filter(|m| !m.is_empty()) {
                if !page::style::media::MediaQueryList::parse(m, None).matches(ctx, &[]) {
                    continue;
                }
            }
            let size = srcset::source_size(dom.get_attribute(child, "sizes"), ctx);
            if let Some((c, density)) = srcset::pick_density(srcset::parse_srcset(set), ctx.dppx, size) {
                return Some((c.url, density));
            }
        }
    }
    srcset::select_density(dom.get_attribute(img, "src"), dom.get_attribute(img, "srcset"), dom.get_attribute(img, "sizes"), ctx)
}

/// The destination this (element, attribute) pair loads with, and whether the load DELAYS THE
/// DOCUMENT'S LOAD EVENT -- or `None` when the pair does not load at all. `<img>` is not here: it
/// loads through [`image_update`], which reads all of its attributes at once.
fn destination_for(dom: &korvus_dom::Dom, id: u32, tag: &str, attr: &str) -> Option<(&'static str, bool)> {
    let table = sources(tag)?;
    let mut dest = table.iter().find(|(a, _)| *a == attr).map(|(_, d)| *d)?;
    let mut delays = delays_load(tag);
    if tag == "input" && !dom.get_attribute(id, "type").is_some_and(|t| t.eq_ignore_ascii_case("image")) {
        return None;
    }
    if tag == "img" || (tag == "source" && parent_is_picture(dom, id).is_some()) {
        return None;
    }
    // A MEDIA BODY IS THE MEDIA ELEMENT'S OWN LOAD: `<video src>`, `<audio src>` and a
    // `<source>` inside either are fetched by the resource selection algorithm in
    // `core/html/media_load.rs`, which reads the response as media rather than firing `load` at the
    // element. A `<source>` in a media element was fetched here as an IMAGE. The poster stays here.
    //
    // AND A `<source>` NEVER LOADS BY ITSELF. Only the algorithm of its parent reads it: a
    // `<picture>`'s image selection (above) or a media element's resource selection. One with no such
    // parent -- React gives a `<source>` its `src` before appending it to its `<video>` -- was fetched
    // here as an IMAGE, whose load held the document's `load` event for a whole hero video
    // (wayfair.com: 74 s live; Brave 151 and Chrome 150 fetch nothing for it).
    if (matches!(tag, "video" | "audio") && attr == "src") || tag == "source" {
        return None;
    }
    if tag == "link" {
        // `rel` is a TOKEN LIST: `rel="preload stylesheet"` -- VitePress's own link,
        // vuejs.org / vitejs.dev / unocss.dev / d3js.org (L19 E8) -- was compared as one string, matched
        // nothing and was never requested. Measured on stock Chrome 150.0.7871.46: it is requested once
        // and applies, as `rel="stylesheet preload"` and `rel=" STYLESHEET "` do; a `stylesheet` token
        // wins over `preload`; `rel="alternate stylesheet"` is requested too (it does not apply --
        // `page::is_stylesheet_link`), and `<link rel=stylesheet disabled>` only by the preload scanner
        // ([`disabled_stylesheet_link`]).
        let rel = dom.get_attribute(id, "rel").unwrap_or("").to_ascii_lowercase();
        let tokens: Vec<&str> = rel.split_ascii_whitespace().collect();
        let has = |t: &str| tokens.contains(&t);
        if !has("stylesheet") && has("preload") {
            // A PRELOAD IS FETCHED AND DOES NOT WAIT. Measured against a 600 ms loopback server:
            // `as=style` and `as=image` are both REQUESTED and both fire `load` at 1 ms, where a real
            // `<link rel=stylesheet>` for the same file fires at 645 ms. The document waits for the
            // element that CONSUMES a preload, never for the hint.
            dest = preload_as_destination(&dom.get_attribute(id, "as").unwrap_or("").to_ascii_lowercase())?;
            delays = false;
        } else {
            let known = ["stylesheet", "prefetch", "prerender", "manifest"];
            dest = link_rel_destination(known.iter().find(|k| has(k))?)?;
            // Only a STYLESHEET delays; prefetch / prerender / manifest all fire `load` at 0-1 ms
            // against the same 600 ms server.
            delays = dest == "style";
        }
    }
    if tag == "use" {
        dest = "";
    }
    Some((dest, delays))
}

/// Everything the element decides, read in one `OpState` borrow: the tag, the destination, the raw
/// URL and the request mode. `None` means "this write induces no request", which is the common case
/// and the reason this runs before anything is allocated.
struct Induced {
    tag: String,
    destination: &'static str,
    url: String,
    mode: &'static str,
    /// Does this load join the document's "delay the load event" set? See [`destination_for`].
    delays: bool,
    /// A frame whose `loading=lazy` defers its navigation (http(s) URLs only -- Blink
    /// `IsFrameLazyLoadable`).
    lazy_frame: bool,
    /// An http(s) URL whose attribute carries the dangling-markup mark ([`dangling_http`]).
    dangling: bool,
}

/// Where `id` lives: the page's document (`Page`), a nested browsing context's document
/// (`Frame(ctx)`), or no document at all -- detached, or a document with no browsing context, such
/// as one `DOMParser` made (`Nowhere`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Home {
    Page,
    Frame(u32),
    Nowhere,
}

/// Whether `id` is CONNECTED: shadow-including, in the page's document or in a frame's. The page
/// test is one walk; only a node outside the page's tree pays for the frame lookup.
pub(crate) fn connected_anywhere(scope: &mut v8::PinScope, id: u32) -> bool {
    home_of(scope, id) != Home::Nowhere
}

pub(crate) fn home_of(scope: &mut v8::PinScope, id: u32) -> Home {
    let root = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let Some(host) = st.try_borrow::<HostDom>() else { return Home::Nowhere };
        let dom = &host.0;
        if dom.is_connected(id) {
            return Home::Page;
        }
        dom.shadow_including_root(id)
    };
    match crate::core::html::browsing_context::frame_of_document_root(scope, root) {
        Some(ctx) => Home::Frame(ctx),
        None => Home::Nowhere,
    }
}

/// [`base_for`], where an element in NO document -- `new Image()` -- takes the base of the realm that is
/// running, which is its node document's: a frame's detached image resolves against the FRAME
/// (Brave 151 and Chrome 150 fetch a frame's `new Image().src = 'p.png'` from the frame's origin).
fn base_for_detached(scope: &mut v8::PinScope, home: Home) -> String {
    match (home, crate::core::html::browsing_context::running_frame(scope)) {
        (Home::Nowhere, Some(ctx)) => base_for(scope, Home::Frame(ctx)),
        (home, _) => base_for(scope, home),
    }
}

/// The base URL a relative URL on `id` resolves against: its own document's. For a nested context's
/// document that is the frame's (`<base>` first, then its URL -- and an `about:blank`/`about:srcdoc`
/// frame's URL is its creator's base URL, measured: a parent `<base href=/sub/>` makes Chrome fetch
/// `/sub/bb.js` for `bb.js` appended to an initial `about:blank` frame's head).
fn base_for(scope: &mut v8::PinScope, home: Home) -> String {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let page_base = crate::core::dom::base_url_of(&op_state.borrow());
    let Home::Frame(ctx) = home else { return page_base };
    let url = crate::core::html::browsing_context::frame_context_url(scope, ctx);
    // an about:blank / about:srcdoc document's fallback base is its creator's, as it was (`Ctx::about_base`)
    let doc_url = if url.is_empty() || has_scheme(&url, "about") {
        crate::core::html::browsing_context::about_base(scope, ctx).unwrap_or(page_base)
    } else {
        url
    };
    let root = crate::core::html::browsing_context::frame_document_root(scope, ctx);
    let st = op_state.borrow();
    let (Some(root), Some(host)) = (root, st.try_borrow::<HostDom>()) else { return doc_url };
    let dom = &host.0;
    match dom.get_elements_by_tag_name_in(root, "base").iter().find_map(|b| dom.get_attribute(*b, "href")) {
        Some(href) => url::Url::parse(&doc_url).ok().and_then(|b| b.join(href).ok()).map(|u| u.to_string()).unwrap_or(doc_url),
        None => doc_url,
    }
}

/// The base URL of node `id`'s NODE DOCUMENT -- what `el.src`, `a.href`, `img.currentSrc` and
/// `node.baseURI` resolve against. Every one of them read the PAGE's base, so a frame's own markup
/// answered with the page's host: YouTube's embed player derives every module URL from its
/// `<script id=base-js src="/s/_/ytembeds/...">`'s `src`, which read `https://dev.to/s/_/ytembeds/...`
/// inside dev.to's embed, and the player threw `Error loading r78Drb: Consecutive load failures`
/// (parity replay). A detached node whose node document is a frame's resolves
/// against the frame too; any other detached node keeps the page's base.
pub(crate) fn node_base_url(scope: &mut v8::PinScope, id: u32) -> String {
    let home = match home_of(scope, id) {
        Home::Nowhere => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            crate::core::dom::node::node_document(scope, &op_state, id)
                .and_then(|d| crate::core::html::browsing_context::context_of_document(scope, d))
                .map_or(Home::Nowhere, Home::Frame)
        }
        h => h,
    };
    base_for(scope, home)
}

/// The URL of node `id`'s NODE DOCUMENT -- a frame's for a node in a frame, as [`node_base_url`] finds it -- the
/// document whose origin an image's CORS-same-origin check compares with (a frame's own
/// same-origin image tainted its canvas, compared with the PAGE's origin).
pub(crate) fn node_document_url(scope: &mut v8::PinScope, id: u32) -> String {
    let home = match home_of(scope, id) {
        Home::Nowhere => {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            crate::core::dom::node::node_document(scope, &op_state, id)
                .and_then(|d| crate::core::html::browsing_context::context_of_document(scope, d))
                .map_or(Home::Nowhere, Home::Frame)
        }
        h => h,
    };
    document_url_for(scope, home)
}

/// The URL of the document `home` lives in -- NOT its base URL. `<base href>` moves where a relative
/// reference POINTS; it does not move which document is "this document", which is the comparison
/// [`same_document`] makes.
fn document_url_for(scope: &mut v8::PinScope, home: Home) -> String {
    if let Home::Frame(ctx) = home {
        let url = crate::core::html::browsing_context::frame_context_url(scope, ctx);
        if !url.is_empty() && !has_scheme(&url, "about") {
            return url;
        }
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    if st.try_borrow::<crate::core::dom::CookieUrl>().is_none() {
        return String::new();
    }
    crate::core::dom::document_url_of(&st)
}

/// Are these two URLs the same document? Compared with their fragments removed, which is the whole
/// difference between a same-document reference and a request.
fn same_document(a: &str, b: &str) -> bool {
    a.split('#').next() == b.split('#').next()
}

fn resolve(base: &str, raw: &str) -> String {
    url::Url::parse(base).ok().and_then(|b| b.join(raw).ok()).map(|u| u.to_string()).unwrap_or_else(|| raw.to_string())
}

fn induced(scope: &mut v8::PinScope, id: u32, attr: &str) -> Option<Induced> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (tag, destination, delays, raw, crossorigin, lazy_attr, srcdoc, module, nomodule) = {
        let st = op_state.borrow();
        let dom = &st.try_borrow::<HostDom>()?.0;
        let tag = dom.tag_name(id)?.to_ascii_lowercase();
        let (destination, delays) = destination_for(dom, id, &tag, attr)?;
        let raw = dom.get_attribute(id, attr)?.trim().to_string();
        let module = tag == "script" && crate::core::html::html_script_element::is_module_element(dom, id);
        let nomodule = tag == "script" && crate::core::html::html_script_element::nomodule_skips_element(dom, id);
        (tag, destination, delays, raw, dom.get_attribute(id, "crossorigin").is_some(), is_lazy(dom, id), dom.get_attribute(id, "srcdoc").is_some(), module, nomodule)
    };
    // A `<script>` WITH NO SCRIPT TYPE IS NEVER REQUESTED. "Prepare the script
    // element" determines the script block's type from `type`/`language` and returns when it is
    // null -- the step ABOVE `nomodule`'s return below, and equally upstream of the fetch.
    // `destination_for` gates `<input type=image>` on its `type` and gated `<script>` on nothing,
    // so any `type` at all put the file on the wire and ran it.
    //
    // WHY THIS IS NOT AN EDGE CASE: it is how jQuery loads a script, and so how a large share of
    // the web does. `$("<script>").attr("src", u).appendTo("head")` reaches `domManip`, which
    // NEUTRALISES the element before inserting it -- `disableScript` rewrites `type` to
    // `"false/" + type` -- runs the URL itself through `_evalUrl`, and restores `type` afterwards
    // (a write that re-prepares nothing). Measured on stock Chrome 150 over the replayed
    // docs.oracle.com/en/java recording, with `Node.appendChild` hooked at document-start:
    //
    // ```text
    //   104 ms  appendChild  type=null      src=…/oracle-universal.js  intoDoc=false
    //   104 ms  script.type= "false/"       src=…/oracle-universal.js
    //   104 ms  appendChild  type="false/"  src=…/oracle-universal.js  intoDoc=TRUE
    // ```
    //
    // Chrome requested that file ONCE (jQuery's own `_evalUrl` XHR) and ran `onMenuContentLoaded`
    // once, from `<anonymous>`. korvus requested it TWICE and ran it twice -- once as
    // `oracle-universal.js`, once as `<anonymous>` -- so two live copies of a 147 KB component held
    // two closures over one DOM, and the one that finished last won. The page then read 1,260 nodes
    // or 386, ON IDENTICAL BYTES, flipping inside a single process.
    if tag == "script" {
        let st = op_state.borrow();
        let runs = st
            .try_borrow::<HostDom>()
            .is_some_and(|h| crate::core::html::html_script_element::element_script_type_runs(&h.0, id));
        if !runs {
            return None;
        }
    }
    // A CLASSIC `<script nomodule>` IS NEVER REQUESTED. HTML's `nomodule` step is inside
    // "prepare the script element", which is upstream of the fetch, so a module-supporting browser
    // does not put the legacy fallback on the wire at all -- measured off Chrome 150's request log,
    // table in `html_script_element::nomodule_skips`. Read from the LIVE arena, which is what makes
    // `s.noModule = true` before `appendChild` suppress the request and the same assignment after it
    // not: by then this has already run.
    if nomodule {
        return None;
    }
    // A PRELOAD WHOSE `media` DOES NOT MATCH IS NOT FETCHED. HTML's preload processing
    // returns before the fetch when "el has a media attribute that does not match the environment".
    // bartleby.com preloads four hero sizes, each behind a width query; Chrome 150 at 1920 px requested
    // only the one that matches and korvus requested all four (`U-preload-media-bg`). A stylesheet is
    // fetched whatever its `media` says (it only does not APPLY), so the test is the preload's alone.
    if tag == "link" {
        let media = {
            let st = op_state.borrow();
            let dom = &st.try_borrow::<HostDom>()?.0;
            let rel = dom.get_attribute(id, "rel").unwrap_or("").to_ascii_lowercase();
            let tokens: Vec<&str> = rel.split_ascii_whitespace().collect();
            (tokens.contains(&"preload") && !tokens.contains(&"stylesheet")).then(|| dom.get_attribute(id, "media").map(str::to_string)).flatten()
        };
        if let Some(q) = media {
            if !q.trim().is_empty() && !crate::core::css::cssom::media_matches(scope, &q) {
                return None;
            }
        }
    }
    let home = if needs_connection(&tag) || frameish(&tag) { home_of(scope, id) } else { Home::Nowhere };
    if needs_connection(&tag) && home == Home::Nowhere {
        return None;
    }
    if raw.is_empty() {
        return None;
    }
    if frameish(&tag) {
        // A frame pointed at `about:...` or a `javascript:` URL does NOT fetch and does NOT get a new
        // document: it STAYS on the initial empty document it was created with. Sending these to the
        // network instead replaced that document asynchronously, which is exactly what
        // `initial-empty-document/initial-content-replacement.html` exists to catch -- it writes into
        // `iframe.contentDocument` and asserts the document is still the same object 100ms later.
        if has_scheme(&raw, "about") || has_scheme(&raw, "javascript") {
            return None;
        }
        // `srcdoc` wins over `src`, as it does in the spec, so a frame carrying both must not fetch.
        if attr == "src" && srcdoc {
            return None;
        }
    }
    // `crossorigin` on the element makes the fetch a CORS request; without it a subresource is
    // no-cors. `track` is the exception -- a text track is always fetched in CORS mode.
    //
    // A FRAME is the other exception, and the bigger one: an `<iframe src>` is a NAVIGATION, not a
    // subresource fetch, and every browser sends `Sec-Fetch-Mode: navigate` for it. This went out as
    // `no-cors`, which is the shape of an image load -- readable by any server that varies on the
    // header, and wrong in exactly the tests (`fetch/metadata/*iframe*`) that exist to read it.
    // A MODULE SCRIPT IS ALWAYS A CORS REQUEST, `crossorigin` or not -- the same rule
    // the runtime's module loader applies to a module's imports, which is why it is stated here rather
    // than inherited from the classic path. It is on the wire: Chrome 150, measured over loopback,
    // sends `Sec-Fetch-Mode: cors` and an `Origin` header for a `<script type=module src>` the page
    // inserted, where korvus sent `no-cors` and no `Origin`.
    //
    // A `<track>`'s CORS settings are its MEDIA ELEMENT's `crossorigin`, and without one it is a
    // SAME-ORIGIN request, not a CORS one (Blink `TextTrackLoader::Load`; HTML "create a potential-CORS
    // request" with the same-origin fallback flag). Measured (Chrome 150): a
    // `<track>` in a plain `<video>` goes out `Sec-Fetch-Mode: same-origin`, one in a `<video
    // crossorigin>` `cors` with an `Origin`; korvus sent both `cors`.
    let mode = if frameish(&tag) {
        "navigate"
    } else if tag == "track" {
        if media_parent_crossorigin(scope, id).is_some() { "cors" } else { "same-origin" }
    } else if crossorigin || module {
        "cors"
    } else {
        "no-cors"
    };
    // RESOLVE AGAINST THE ELEMENT'S DOCUMENT'S BASE URL (a nested context's own).
    // The kernel's loader joins whatever it is handed against the page LOCATION, which
    // is the document url and not the base url -- so before this, `<base href="/sub/">` moved nothing
    // that was actually fetched. Handing over an ALREADY-ABSOLUTE url makes the kernel's join a no-op.
    // Resolution is deliberately AFTER the `about:`/`javascript:` tests above, which read the raw
    // attribute; an attribute that does not resolve travels on unchanged, and fails downstream
    // exactly as it did.
    let url = resolve(&base_for(scope, home), &raw);
    // A STYLESHEET WHOSE URL DOES NOT PARSE IS NOT FETCHED AND FIRES NOTHING (HTML "fetch and process the linked
    // resource": "if url is failure, then return"). Chrome 152.0.7977.82, 2026-10-23: `href="http://["` makes no request,
    // fires neither `load` nor `error` and has no sheet; this engine put it on the wire, fired `error` and listed a
    // resource-timing entry named `http://[`.
    if tag == "link" && destination == "style" {
        let unloadable = !chrome_parses(&url);
        link_sheet_unloadable(&op_state, id, unloadable);
        if unloadable {
            return None;
        }
    }
    // AN SVG REFERENCE IS A URL PLUS A FRAGMENT, AND THE FRAGMENT IS NOT PART OF THE REQUEST.
    // `<use href>` names a shadow tree to clone; SVG 2 resolves the reference and then splits it,
    // and korvus was putting the whole string on the wire and asking for it once per ELEMENT.
    //
    // Measured on stock Chrome 150.0.7871.46 over a loopback server (`Cache-Control: no-store`):
    //
    // ```text
    //   three <use href="#s"> into a <symbol> in this document      0 requests   korvus: 3
    //   <use href="#nope">, naming nothing at all                   0 requests   korvus: 1
    //   three <use href="sp.svg#a|#b|#a">, an external sprite       1 request    korvus: 3
    //   two  <use href="s3.svg">, no fragment at all                1 request
    // ```
    //
    // Both halves are one rule: a reference whose document is THIS document is resolved in the
    // tree that is already here, and one whose document is another is a fetch OF THAT DOCUMENT,
    // which every fragment of it then shares. On the corpus that is 937 requests for a page's own
    // URL (`carto.com` asked for itself 49 times) and 405 for a sprite (`docs.nginx.com` asked for
    // one SVG 187 times); Chrome asked once for the sprite and never for the page.
    let url = if tag == "use" {
        let document = document_url_for(scope, home);
        let target = url.split('#').next().unwrap_or(&url).to_string();
        if target.is_empty() || same_document(&target, &document) {
            return None;
        }
        target
    } else {
        url
    };
    // Blink `IsFrameLazyLoadable`: only an http(s) URL is ever deferred, and only in the page's own
    // document (a nested document is not laid out here, so there is no viewport to be near).
    let lazy_frame = frameish(&tag) && lazy_attr && home == Home::Page && (has_scheme(&url, "http") || has_scheme(&url, "https"));
    let dangling = dangling_http(&raw, &url);
    Some(Induced { tag, destination, url, mode, delays, lazy_frame, dangling })
}

/// Whether an element's load of `url`, resolved from attribute value `raw` (its HTML spaces stripped, as Blink strips
/// them before it parses), is refused for dangling markup (`url::url::potentially_dangling_markup`): `raw`
/// is marked and `url` is http(s).
fn dangling_http(raw: &str, url: &str) -> bool {
    crate::modules::url::url::potentially_dangling_markup(raw) && (has_scheme(url, "http") || has_scheme(url, "https"))
}

/// Start the load `id` induces through `attr`, if any. Returns nothing: the result reaches the page as
/// a `load` or `error` event on the element, which is the only channel a browser gives it.
///
/// `eager` is set by the lazy-load check ([`crate::core::html::lazy_load`]) when a deferred element
/// has come near the viewport; every other caller passes `false`.
pub(crate) fn maybe_load(scope: &mut v8::PinScope, id: u32, attr: &str, eager: bool) {
    maybe_load_for(scope, id, attr, eager, Cause::Inserted)
}

/// What put a load in question. It matters to exactly one rule, the iframe load event steps for a
/// frame whose `src` fetches nothing ([`blank_frame_load`]).
#[derive(Clone, Copy, PartialEq)]
enum Cause {
    /// The element was inserted.
    Inserted,
    /// The element was parsed into its document ([`sweep`]). An insertion, except to a `disabled` stylesheet link
    /// ([`disabled_stylesheet_link`]).
    Parsed,
    /// An attribute was written on a connected element.
    Written,
    /// A frame's `loading` attribute changed, which re-evaluates its `src` but navigates nowhere.
    Rerouted,
}

fn maybe_load_for(scope: &mut v8::PinScope, id: u32, attr: &str, eager: bool, cause: Cause) {
    enum Route {
        Image(u32),
        Icon,
        Link,
        Element,
    }
    let route = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let Some(host) = st.try_borrow::<HostDom>() else { return };
        let dom = &host.0;
        match dom.tag_name(id).map(|t| t.to_ascii_lowercase()).as_deref() {
            // Only the attributes "update the image data" reads start one: a class or alt write on an
            // image is not a relevant mutation.
            Some("img") if IMG_ATTRS.contains(&attr) => Route::Image(id),
            Some("img") => return,
            Some("source") if PICTURE_SOURCE_ATTRS.contains(&attr) => match parent_is_picture(dom, id).and_then(|p| picture_img(dom, p)) {
                Some(img) => Route::Image(img),
                None => Route::Element,
            },
            // `loading` on a frame: an eager frame whose load was deferred loads now.
            Some("iframe") | Some("frame") if attr == "loading" => {
                drop(st);
                return maybe_load_for(scope, id, "src", false, Cause::Rerouted);
            }
            // A `<link>` whose `rel` names an icon is the favicon handler's, not the element loader's.
            Some("link") if dom.get_attribute(id, "rel").is_some_and(crate::core::html::link_icons::is_icon_rel) => Route::Icon,
            Some("link") => Route::Link,
            _ => Route::Element,
        }
    };
    match route {
        Route::Image(img) => {
            if eager {
                image_update(scope, img, true);
            } else {
                schedule_image_update(scope, img);
            }
            return;
        }
        Route::Icon => {
            crate::core::html::link_icons::link_changed(scope);
            return;
        }
        // A link that stopped being an icon changes the favicon list as well.
        Route::Link => crate::core::html::link_icons::link_changed(scope),
        Route::Element => {}
    }
    // A `<link>`'s `rel` decides what its `href` is: the loadCSS pattern
    // `rel=preload as=style onload="this.rel='stylesheet'"` turns a preload into a stylesheet, which
    // Chrome 150 then applies -- from the preloaded response, with no second request.
    let attr = if matches!(route, Route::Link) && attr == "rel" { "href" } else { attr };
    // A FRAME THAT FETCHES NOTHING STILL ENDS A NAVIGATION. `induced` returns `None`
    // for a frame with no `src`, an empty one, `about:*` or `javascript:` -- correctly, there is no
    // request -- and that used to be the whole answer, so nothing ever happened. HTML's "process the
    // iframe attributes" runs the IFRAME LOAD EVENT STEPS for exactly those URLs instead of
    // navigating, so the element gets its `load`.
    if blank_frame_load(scope, id, attr, cause) {
        return;
    }
    let Some(ind) = induced(scope, id, attr) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // A stylesheet `<link disabled>` a script inserted loads nothing until it is enabled; the parser's is fetched
    // ([`disabled_stylesheet_link`]).
    if cause != Cause::Parsed && ind.tag == "link" && ind.destination == "style" && ind.delays && op_state.borrow().try_borrow::<HostDom>().is_some_and(|h| disabled_stylesheet_link(&h.0, id)) {
        return;
    }
    // A script element is prepared ONCE ("already started"): a moved, re-inserted or cloned script,
    // or one whose `src` changes after it started, never fetches again.
    if ind.tag == "script" {
        if crate::core::html::html_script_element::script_started(&op_state, id) {
            return;
        }
        crate::core::html::html_script_element::mark_script_started(&op_state, id);
    }
    if claimed(&op_state, id, &claim_key(&ind.tag, ind.destination, ind.delays, &ind.url)) {
        return;
    }
    if ind.lazy_frame && !eager {
        crate::core::html::lazy_load::defer(scope, id, crate::core::html::lazy_load::Kind::Frame);
        return;
    }
    issue(scope, id, &ind.tag, &ind.url, ind.destination, ind.mode, ind.delays, ind.dangling);
}

/// A frame element that is IN a document and whose `src` induces no fetch stays on its initial
/// empty document -- and HTML runs the IFRAME LOAD EVENT STEPS for it: `load` at the ELEMENT.
/// Returns whether it fired, so the caller skips the fetch path it has no request for.
///
/// MEASURED ON CHROME 150 RATHER THAN READ OFF THE SPEC, both engines against one loopback fixture,
/// korvus before this in brackets. Every non-fetching shape fires exactly once:
///
/// ```text
///   no src attribute at all                     1   [0]
///   src=""                                      1   [0]
///   src="about:blank"  · "about:blank?a=1#f"    1   [0]
///   src="about:srcdoc" · "about:foo"            1   [0]
///   src="javascript:void 0"                     1   [0]
///   <frame> rather than <iframe>                1   [0]
///   srcdoc="<p>hi"                              1   [1]   <- a real navigation; already fired
/// ```
///
/// AT THE ELEMENT AND NOT AT THE FRAME'S WINDOW, which is the half a reading gets wrong: with a
/// listener on a connected frame's `contentWindow` and a second `src="about:blank"` written at it,
/// Chrome fires the ELEMENT's `load` once and the window's **zero** times, while replacing the
/// frame's document (`contentDocument` differs, `contentWindow` is the same object).
///
/// SYNCHRONOUSLY, as part of the insertion: appending an `<iframe>` logs its `load` BEFORE the
/// statement after `appendChild`, and a parser-made `<iframe id=a>` has already fired by the time
/// the next inline script can add a listener (so Chrome's own answer for "a late listener hears it"
/// is no). This engine parses a document in one shot, so its parser-made frames all fire at the
/// install sweep rather than interleaved with the inline scripts -- the same ordering limit
/// `document.write` has here, and not a second one.
///
/// RE-ENTRY IS THE RULE, NOT AN ACCIDENT: each insertion is a new navigation, so moving a frame
/// fires again (Chrome: 2 for remove+append, 2 for insert-then-`src="about:blank"`).
///
/// ONLY THE INSERTION IS SYNCHRONOUS. HTML runs the iframe load event steps in-line for the INITIAL
/// insertion alone; a `src` written on a frame that is already in a document is a navigation, and
/// its `load` arrives in a later task. Firing that one in-line too made
/// `iframe.onload = () => iframe.src = "about:blank"` recurse without bound -- native frames under
/// every level, so it overflowed the stack PAST V8's margin and took the process down
/// (`Check failed: IsOnCentralStack()`, WPT `ignore-opens-during-unload.window.js`) instead of
/// throwing a `RangeError`. Two writes before that task are one navigation replacing the other, so
/// they fire once.
fn blank_frame_load(scope: &mut v8::PinScope, id: u32, attr: &str, cause: Cause) -> bool {
    // Only the source attribute. The attribute seam calls this for every write, and a `width` or a
    // `class` on a frame is not a navigation.
    if attr != "src" {
        return false;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (frame, raw, srcdoc) = {
        let st = op_state.borrow();
        let Some(host) = st.try_borrow::<HostDom>() else { return false };
        let dom = &host.0;
        let Some(tag) = dom.tag_name(id).map(|t| t.to_ascii_lowercase()) else { return false };
        (
            frameish(&tag),
            dom.get_attribute(id, "src").map(|s| s.trim().to_string()).unwrap_or_default(),
            dom.get_attribute(id, "srcdoc").is_some(),
        )
    };
    // `srcdoc` wins over `src` in the spec and in the measurement above: the `about:srcdoc`
    // navigation is a real one and fires its own `load`. Firing here too would double it.
    if !frame || srcdoc {
        return false;
    }
    if !(raw.is_empty() || has_scheme(&raw, "about") || has_scheme(&raw, "javascript")) {
        return false;
    }
    // A frame that is in no document has no navigable and fires nothing -- Chrome's answer for an
    // `<iframe>` appended to a detached `<div>` is 0 until the div is connected, and 1 then.
    if home_of(scope, id) == Home::Nowhere {
        return false;
    }
    match cause {
        Cause::Inserted | Cause::Parsed => {
            let el = wrap_cached(scope, &op_state, id);
            crate::core::html::browsing_context::fire_initial_blank_load(scope, id, el.into());
        }
        Cause::Written => queue_blank_frame_load(scope, id),
        // A `loading` change on a frame that fetches nothing navigates nowhere.
        Cause::Rerouted => {}
    }
    true
}

/// Frames with a non-initial about:/javascript: navigation whose `load` is queued, one per element.
#[derive(Default)]
struct PendingBlankLoads(HashSet<u32>);

fn queue_blank_frame_load(scope: &mut v8::PinScope, id: u32) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<PendingBlankLoads>().is_none() {
            st.put(PendingBlankLoads::default());
        }
        if !st.borrow_mut::<PendingBlankLoads>().0.insert(id) {
            return;
        }
    }
    crate::core::intersection_observer::rendering::queue_task_with_id(scope, cb_blank_frame_load, id as u64, 0.0);
}

fn cb_blank_frame_load(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(id) = crate::binding::protos::body_data(scope, &args).number_value(scope).map(|n| n as u32) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let queued = op_state.borrow_mut().try_borrow_mut::<PendingBlankLoads>().map(|p| p.0.remove(&id)).unwrap_or(false);
    // Removed from its document meanwhile: the navigation went with the navigable.
    if !queued || home_of(scope, id) == Home::Nowhere {
        return;
    }
    let el = wrap_cached(scope, &op_state, id);
    fire_at(scope, el.into(), "load");
}

/// The `<img>`s whose "update the image data" is queued for the next microtask checkpoint.
#[derive(Default)]
struct ImageUpdates(HashSet<u32>);

/// The `<img>`s with a request in flight: their `complete` reads `false` until the request settles
/// with `load` or `error`. HTML "the complete attribute": `complete` is `true` only when there is no
/// `src`/`srcset`, or the current request's state is "completely available" or "broken" with no
/// pending request. Measured, Chrome 150 (`ACC-img-complete`): `new Image()` is `complete` true;
/// `i.src = u` makes it `false` synchronously and it returns to `true` when that request's `load` or
/// `error` fires. Before this, `complete` was hardcoded `true`, so an image test that waits for the
/// load to finish by polling `complete` saw it done before the fetch had even started.
#[derive(Default)]
struct ImagesLoading(HashSet<u32>);

/// The `<img>`s with an `error` event queued for a source that names nothing, each with the load-delay
/// token it holds. Blink's `ImageLoader::QueuePendingErrorEvent` posts the event as a DOM-manipulation
/// task that delays the document's load event until it has fired (`IncrementLoadEventDelayCount`),
/// and a second one for the same element replaces the first -- one event, not two (crbug.com/722500).
#[derive(Default)]
struct PendingImageErrors(HashMap<u32, u32>);

/// Queue `img`'s `error` event for a source that names nothing (see [`image_update`]).
fn queue_image_error(scope: &mut v8::PinScope, img: u32) {
    let token = new_load_delay_token(scope);
    delay_load(scope, token);
    let replaced = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<PendingImageErrors>().is_none() {
            st.put(PendingImageErrors::default());
        }
        st.borrow_mut::<PendingImageErrors>().0.insert(img, token)
    };
    // the replaced event's task finds this entry and fires once; its own token is released here
    if let Some(old) = replaced {
        undelay_load(scope, old);
    }
    crate::core::intersection_observer::rendering::queue_task_with_id(scope, cb_image_error, img as u64, 0.0);
}

/// Drop `img`'s queued `error` event, if any: a request for a real source has started since.
fn cancel_image_error(scope: &mut v8::PinScope, img: u32) {
    let token = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        st.try_borrow_mut::<PendingImageErrors>().and_then(|p| p.0.remove(&img))
    };
    if let Some(token) = token {
        undelay_load(scope, token);
    }
}

fn cb_image_error(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(img) = crate::binding::protos::body_data(scope, &args).number_value(scope).map(|n| n as u32) else { return };
    let token = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        st.try_borrow_mut::<PendingImageErrors>().and_then(|p| p.0.remove(&img))
    };
    let Some(token) = token else { return };
    fire(scope, img, "error");
    undelay_load(scope, token);
}

/// Mark `<img>` `img` as having a request in flight (its `complete` now reads `false`).
fn mark_image_loading(state: &mut OpState, img: u32) {
    if state.try_borrow::<ImagesLoading>().is_none() {
        state.put(ImagesLoading::default());
    }
    state.borrow_mut::<ImagesLoading>().0.insert(img);
}

/// Mark `<img>` `img` as settled -- its request reached `load` or `error`, or its bytes were already
/// available (so `complete` reads `true`). Harmless for a non-image id, which is never in the set.
fn mark_image_settled(state: &mut OpState, img: u32) {
    if let Some(s) = state.try_borrow_mut::<ImagesLoading>() {
        s.0.remove(&img);
    }
}

/// `HTMLImageElement.complete`: `true` when the element has no request (no `src`/`srcset`) or its
/// request has settled, `false` while a request is in flight.
pub(crate) fn image_is_complete(scope: &mut v8::PinScope, img: u32) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let Some(host) = st.try_borrow::<HostDom>() else { return true };
    let src = host.0.get_attribute(img, "src").map(|s| s.trim().to_string()).unwrap_or_default();
    let srcset = host.0.get_attribute(img, "srcset").map(|s| s.trim().to_string()).unwrap_or_default();
    if src.is_empty() && srcset.is_empty() {
        return true;
    }
    !st.try_borrow::<ImagesLoading>().is_some_and(|s| s.0.contains(&img))
}

/// HTML's "update the image data" runs in a MICROTASK for a script-driven change, so only the state
/// at the end of the task is requested. Measured on Chrome 150: `img.src = a; img.srcset = "b 1x"` in
/// one task requests `b` only; the same two writes 300 ms apart request both.
fn schedule_image_update(scope: &mut v8::PinScope, img: u32) {
    // AVAILABLE AT ONCE, NOT AT THE MICROTASK: HTML's "update the image data" takes an
    // entry of the list of available images synchronously, which is why a carousel that clones a
    // loaded logo and reads `scrollWidth` in the same loop sees it grow in Chrome.
    let avail = crate::core::html::canvas_image::img_layout_size(scope, img);
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        match avail {
            // An image this document already has is available at once with its size, and `complete`.
            Some(size) => {
                crate::core::dom::geometry_queries::NaturalImages::set(&mut st, img, Some(page::ImageState::Available(size.0, size.1)));
                mark_image_settled(&mut st, img);
            }
            // A new selection: `complete` reads `false` from this synchronous point until it settles.
            None => mark_image_loading(&mut st, img),
        }
    }
    let fresh = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<ImageUpdates>().is_none() {
            st.put(ImageUpdates::default());
        }
        st.borrow_mut::<ImageUpdates>().0.insert(img)
    };
    if !fresh {
        return;
    }
    // THE QUEUED UPDATE ALREADY DELAYS THE LOAD EVENT, as Blink's `delay_until_do_update_from_element_`
    // does: a script that sets `img.src` and ends must not let `load` fire in the gap before the
    // microtask issues the fetch (which takes its own token).
    let token = new_load_delay_token(scope);
    delay_load(scope, token);
    let arr = v8::Array::new(scope, 2);
    let a = v8::Integer::new_from_unsigned(scope, img);
    arr.set_index(scope, 0, a.into());
    let t = v8::Integer::new_from_unsigned(scope, token);
    arr.set_index(scope, 1, t.into());
    let f = build_fn_data(scope, cb_image_update, "", 0, arr.into());
    scope.enqueue_microtask(f);
}

fn cb_image_update(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(arr) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let Some(img) = arr.get_index(scope, 0).and_then(|v| v.uint32_value(scope)) else { return };
    let Some(token) = arr.get_index(scope, 1).and_then(|v| v.uint32_value(scope)) else { return };
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if let Some(u) = st.try_borrow_mut::<ImageUpdates>() {
            u.0.remove(&img);
        }
    }
    image_update(scope, img, false);
    undelay_load(scope, token);
}

/// Select, resolve, defer or load: one `<img>`'s request, if its current state induces one.
///
/// THE SELECTION IS A REQUEST ONLY WHEN IT CHANGED. `img.src = a` and, later, `img.srcset = "b 2x"`
/// at DPR 1 still select `a` (src is the 1x candidate) and Chrome requests nothing more; at DPR 2 the
/// selection moves to `b` and Chrome requests it. [`claim`] is that test: a URL this element already
/// requested is not requested again.
pub(crate) fn image_update(scope: &mut v8::PinScope, img: u32, eager: bool) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (raw, lazy, crossorigin) = {
        let st = op_state.borrow();
        let media = crate::core::dom::geometry_queries::media_context(&st);
        let Some(host) = st.try_borrow::<HostDom>() else { return };
        let dom = &host.0;
        (image_source_density(dom, img, &media), is_lazy(dom, img), dom.get_attribute(img, "crossorigin").is_some())
    };
    let density = raw.as_ref().map_or(1.0, |(_, d)| *d);
    let Some(raw) = raw.map(|(r, _)| r.trim().to_string()).filter(|r| !r.is_empty()) else {
        crate::core::html::lazy_load::forget(scope, img);
        crate::core::dom::geometry_queries::NaturalImages::set_source(&mut op_state.borrow_mut(), img, None);
        // NOTHING TO FETCH (A/B C8). A `src` that names nothing -- `src=""`, or white space -- is a
        // failed load: Blink's `ImageLoader::DoUpdateFromElement` queues an `error` event for a
        // source URL that is empty but not null, and `ResetLayoutDisposition` lays the image out as
        // its fallback content. Chrome 150: `<img src="" alt=x>` fires `error` and is 23.97x24. With
        // no `src` at all nothing fires, and the layout decides from the alt text alone
        // (`BoxTree::image_fallback`).
        let has_src = {
            let st = op_state.borrow();
            st.try_borrow::<HostDom>().is_some_and(|h| h.0.get_attribute(img, "src").is_some())
        };
        {
            let mut st = op_state.borrow_mut();
            let state = has_src.then_some(page::ImageState::Broken);
            crate::core::dom::geometry_queries::NaturalImages::set(&mut st, img, state);
            mark_image_settled(&mut st, img);
        }
        if has_src {
            queue_image_error(scope, img);
        } else {
            cancel_image_error(scope, img);
        }
        return;
    };
    let home = home_of(scope, img);
    let url = resolve(&base_for_detached(scope, home), &raw);
    // what the image is read under from here on: the candidate selected, not its `src`
    crate::core::dom::geometry_queries::NaturalImages::set_source(&mut op_state.borrow_mut(), img, Some((url.clone(), density)));
    // THE LIST OF AVAILABLE IMAGES: an image this document already has is available the
    // moment an element names it -- a clone of a loaded logo, `new Image()` with the same `src`, a lazy
    // one included -- with its natural size, before (and whether or not) anything is requested.
    // Chrome 150, measured (`U-available-images`): all four are `complete` and 120 wide at once.
    if let Some(size) = crate::core::html::canvas_image::img_layout_size(scope, img) {
        let mut st = op_state.borrow_mut();
        crate::core::dom::geometry_queries::NaturalImages::set(&mut st, img, Some(page::ImageState::Available(size.0, size.1)));
        mark_image_settled(&mut st, img);
    }
    if claimed(&op_state, img, &url) {
        return;
    }
    // A LAZY IMAGE IS DEFERRED WHEREVER IT IS -- detached included. Measured: `new Image()` with
    // `loading = "lazy"` and a `src` is never requested (Blink observes it and a detached element
    // never intersects). An image in a nested document is loaded eagerly: nothing lays that
    // document out here, so there is no viewport for it to be near (ruling, see `lazy_load.rs`).
    //
    // A NEW REQUEST, DEFERRED OR IN FLIGHT, MAKES THE IMAGE "POTENTIALLY AVAILABLE" AGAIN (A/B C8): a
    // broken one is laid out as an image again, not as its fallback content (Blink's
    // `ResetLayoutDisposition` -> `EnsurePrimaryContent`). Blink does it at the synchronous `src`
    // write already; here it is at the request, which a re-set of the same failed URL never reaches.
    if lazy && !eager && !matches!(home, Home::Frame(_)) {
        crate::core::dom::geometry_queries::NaturalImages::unbreak(&mut op_state.borrow_mut(), img);
        crate::core::html::lazy_load::defer(scope, img, crate::core::html::lazy_load::Kind::Image);
        return;
    }
    crate::core::html::lazy_load::forget(scope, img);
    // A request is now in flight: `complete` reads `false` until its `load`/`error` fires.
    mark_image_loading(&mut op_state.borrow_mut(), img);
    crate::core::dom::geometry_queries::NaturalImages::unbreak(&mut op_state.borrow_mut(), img);
    cancel_image_error(scope, img);
    // A LAZY IMAGE NEVER HOLDS ITS DOCUMENT'S `load`. One in a frame's document is requested at once (the
    // ruling above), but Chrome requests even a lazy image IN the viewport only after `load` (`lazy_load.rs`'s table,
    // +5..20 ms), so none ever delays it. A frame's `load` waits for the loads its document
    // starts, so this request held the frame's `load`. The WPT frames `image-loading-lazy-in-cross-origin-iframe-00{1,2}`
    // then posted `image_loaded` before `window_loaded`, which Chrome never does.
    let delays = !(lazy && matches!(home, Home::Frame(_)));
    let dangling = dangling_http(&raw, &url);
    issue(scope, img, "img", &url, "image", if crossorigin { "cors" } else { "no-cors" }, delays, dangling);
}

/// What [`claim`] records a load under. A `<link>` can start TWO loads of one URL -- a preload and,
/// after `rel` becomes `stylesheet`, the stylesheet -- so its preload and its stylesheet load are
/// claimed apart (a stylesheet load is the one that delays the load event); every other element claims
/// by URL alone.
fn claim_key(tag: &str, destination: &str, delays: bool, url: &str) -> String {
    match (tag, destination, delays) {
        ("link", "style", true) => format!("stylesheet {url}"),
        ("link", "style", false) => format!("preload {url}"),
        _ => url.to_string(),
    }
}

/// A document's `<link rel=preload as=style>` responses not yet consumed, by `"<mode> <url>"`: the
/// fetch's promise. HTML's "consume a preloaded resource" -- a stylesheet load of the same URL and mode
/// joins it instead of requesting again. Measured on stock Chrome 150.0.7871.46 over a loopback server
/// answering `Cache-Control: no-store`: a preload followed by `link.rel = "stylesheet"` (loadCSS), and
/// a preload beside a `<link rel=stylesheet>` of the same file, are each ONE request.
#[derive(Default)]
struct StylePreloads(HashMap<String, v8::Global<v8::Promise>>);

/// One response a PARSER-time fetch brought back (the document's `<script src>`s and stylesheets).
pub struct ParserFetch {
    pub url: String,
    pub destination: &'static str,
    pub mode: String,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// The parser-time responses, keyed like a shared request, for the one sweep that starts the parsed
/// document's other loads: a `<link rel=preload>` of a resource the parser already fetched
/// is answered from it. It was a second request for the same bytes -- every preloaded script and
/// stylesheet, twice (airbnb: ~40 scripts and a 1.7 MB sheet). Chrome: one.
#[derive(Default)]
struct ParserFetched(HashMap<String, ParserFetch>);

pub(crate) fn stage_parser_fetches(scope: &mut v8::PinScope, fetches: Vec<ParserFetch>) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let map = fetches.into_iter().map(|f| (request_key(&f.url, f.destination, &f.mode), f)).collect();
    st.put(ParserFetched(map));
}

pub(crate) fn clear_parser_fetches(scope: &mut v8::PinScope) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<ParserFetched>().is_some() {
        st.take::<ParserFetched>();
    }
    if st.try_borrow::<ParserInFlight>().is_some() {
        st.take::<ParserInFlight>();
    }
}

/// The parser-time requests still in flight when the parsed document's other loads start (the
/// streaming parse): a `<link rel=preload>` of one joins it -- one request, as Chrome's -- through a promise
/// [`settle_parser_fetch`] settles when the response is in.
#[derive(Default)]
struct ParserInFlight(std::collections::HashSet<String>);

/// The promises handed to preloads that joined an in-flight parser request, by request key.
#[derive(Default)]
struct ParserJoined(HashMap<String, Vec<v8::Global<v8::PromiseResolver>>>);

pub(crate) fn stage_parser_in_flight(scope: &mut v8::PinScope, keys: Vec<(String, &'static str, String)>) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    st.put(ParserInFlight(keys.iter().map(|(u, d, m)| request_key(u, d, m)).collect()));
}

/// A parser request is in: the preloads that joined it get its response (`None`: it failed).
pub(crate) fn settle_parser_fetch(scope: &mut v8::PinScope, url: &str, destination: &'static str, mode: &str, response: Option<ParserFetch>) {
    let joined = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        st.try_borrow_mut::<ParserJoined>().and_then(|j| j.0.remove(&request_key(url, destination, mode))).unwrap_or_default()
    };
    for r in joined {
        let r = v8::Local::new(scope, &r);
        match response.as_ref().and_then(|f| staged_response(scope, f)) {
            Some(p) => {
                r.resolve(scope, p.into());
            }
            None => {
                let msg = str_val(scope, "Failed to fetch");
                let e = v8::Exception::type_error(scope, msg.try_into().unwrap_or_else(|_| v8::String::empty(scope)));
                r.reject(scope, e);
            }
        }
    }
}

/// A preload of a parser request still in flight: the promise its response settles ([`settle_parser_fetch`]).
fn join_parser_in_flight<'s>(scope: &mut v8::PinScope<'s, '_>, key: &str) -> Option<v8::Local<'s, v8::Promise>> {
    let in_flight = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<ParserInFlight>().is_some_and(|f| f.0.contains(key))
    };
    if !in_flight {
        return None;
    }
    let r = v8::PromiseResolver::new(scope)?;
    let g = v8::Global::new(scope, r);
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<ParserJoined>().is_none() {
            st.put(ParserJoined::default());
        }
        st.borrow_mut::<ParserJoined>().0.entry(key.to_string()).or_default().push(g);
    }
    Some(r.get_promise(scope))
}

/// A settled promise of the response object a fetch hands its loader (`{status, url, headers, body}`).
fn staged_response<'s>(scope: &mut v8::PinScope<'s, '_>, f: &ParserFetch) -> Option<v8::Local<'s, v8::Promise>> {
    let o = v8::Object::new(scope);
    let status: v8::Local<v8::Value> = v8::Integer::new(scope, f.status as i32).into();
    crate::binding::protos::set_prop(scope, o, "status", status);
    let u = str_val(scope, &f.url);
    crate::binding::protos::set_prop(scope, o, "url", u);
    let headers = v8::Array::new(scope, f.headers.len() as i32);
    for (i, (k, v)) in f.headers.iter().enumerate() {
        let pair = v8::Array::new(scope, 2);
        let kv = str_val(scope, k);
        pair.set_index(scope, 0, kv);
        let vv = str_val(scope, v);
        pair.set_index(scope, 1, vv);
        headers.set_index(scope, i as u32, pair.into());
    }
    crate::binding::protos::set_prop(scope, o, "headers", headers.into());
    let b = str_val(scope, &f.body);
    crate::binding::protos::set_prop(scope, o, "body", b);
    let resolver = v8::PromiseResolver::new(scope)?;
    resolver.resolve(scope, o.into());
    Some(resolver.get_promise(scope))
}

/// AN IMAGE DOCUMENT'S OWN `<img>` IS FED FROM ITS NAVIGATION'S RESPONSE, NEVER REQUESTED. Blink's
/// `ImageDocumentParser` makes the document's image from the main resource's bytes -- an `ImageResource` with no
/// `ResourceLoader` -- and `HTMLImageElement::StartLoadingImageDocument` hands it to the element's loader, which takes it
/// once instead of fetching (`ImageLoader::UpdateFromElement`: "Prevent the creation of a ResourceLoader (and therefore a
/// network request) for ImageDocument loads"). With no loader there is no resource-timing entry, no CSP check and no HTTP
/// status check (`ResourceLoader::DidReceiveResponse` is what fails a 4xx). korvus built the document as markup
/// (`text_viewer::image_document`), and its `<img src>` asked the network again: two GETs for one image, top-level and
/// framed, where Chrome 150 makes one.
///
/// The navigation's bytes are kept as an image's (`spec::FetchedImages` under `(url, "no-cors")`): a frame's by the
/// subresource loader that fetched its navigation (korvus-runtime), the page's by [`arm_page_image_document`]. Each image
/// document's URL waits here until its `<img>` takes it ([`take_image_document`]); a later load of that URL is an
/// ordinary one, as in Blink.
#[derive(Default)]
struct ImageDocumentContent {
    /// The page's next document's: armed before that document is installed, moved to `open` by [`reset_started_loads`].
    next_page: Option<String>,
    /// Each image document's URL not yet taken, by its browsing context (`None`: the page's).
    open: HashMap<Option<u32>, String>,
}

/// The page's next document is the image document of `url`, built from `bytes` ([`ImageDocumentContent`]).
pub fn arm_page_image_document(op_state: &std::rc::Rc<RefCell<OpState>>, url: &str, headers: &[(String, String)], bytes: &[u8]) {
    let Ok(url) = url::Url::parse(url).map(|u| u.to_string()) else { return };
    let mut st = op_state.borrow_mut();
    // Status 200: the image document's resource has no loader to fail an HTTP error status (see above).
    if let Some(images) = st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::FetchedImages>()) {
        let img = spec::FetchedImage { status: 200, final_url: url.clone(), headers: headers.to_vec(), bytes: std::sync::Arc::new(bytes.to_vec()) };
        images.insert(&url, "no-cors", img);
    }
    if st.try_borrow::<ImageDocumentContent>().is_none() {
        st.put(ImageDocumentContent::default());
    }
    st.borrow_mut::<ImageDocumentContent>().next_page = Some(url);
}

/// The frame `ctx`'s new document: the image document of `url`, or (`None`) any other.
pub(crate) fn begin_frame_image_document(scope: &mut v8::PinScope, ctx: u32, url: Option<&str>) {
    let url = url.and_then(|u| url::Url::parse(u).ok()).map(|u| u.to_string());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    if url.is_none() && st.try_borrow::<ImageDocumentContent>().is_none() {
        return;
    }
    if st.try_borrow::<ImageDocumentContent>().is_none() {
        st.put(ImageDocumentContent::default());
    }
    let open = &mut st.borrow_mut::<ImageDocumentContent>().open;
    match url {
        Some(u) => open.insert(Some(ctx), u),
        None => open.remove(&Some(ctx)),
    };
}

/// The image document's response, when `id` is that document's `<img>` loading its URL for the first time
/// ([`ImageDocumentContent`]); `None` for every other load. `None` too when the kept bytes are gone (the image store is
/// bounded): the element then asks the network, as it did before.
fn take_image_document(scope: &mut v8::PinScope, id: u32, url: &str) -> Option<ParserFetch> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let named = op_state.borrow().try_borrow::<ImageDocumentContent>().is_some_and(|c| c.open.values().any(|u| u == url));
    if !named {
        return None;
    }
    let key = match home_of(scope, id) {
        Home::Page => None,
        Home::Frame(ctx) => Some(ctx),
        Home::Nowhere => return None,
    };
    let mut st = op_state.borrow_mut();
    let open = &mut st.try_borrow_mut::<ImageDocumentContent>()?.open;
    if open.get(&key).map(String::as_str) != Some(url) {
        return None;
    }
    open.remove(&key);
    let f = st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::FetchedImages>())?.get(url, "no-cors")?;
    Some(ParserFetch { url: f.final_url, destination: "image", mode: "no-cors".to_string(), status: f.status, headers: f.headers, body: String::new() })
}

/// Issue one element load: claim it, take its load-delay token, fetch, and wire the completion.
fn issue(scope: &mut v8::PinScope, id: u32, tag: &str, url: &str, destination: &'static str, mode: &'static str, delays: bool, dangling: bool) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if !claim(&op_state, id, &claim_key(tag, destination, delays, url)) {
        return;
    }
    if tag == "script" {
        enlist_if_in_order(scope, id);
    }
    let framing = frameish(tag);
    // A recursive or over-limit frame load is never issued: the frame keeps its initial document.
    if framing && crate::core::html::browsing_context::frame_url_refused(scope, id, url) {
        return;
    }
    // Nor is a frame navigation to a url with dangling markup: `FrameLoader::StartNavigation` drops it, and
    // the frame keeps its initial document.
    if framing && dangling {
        return;
    }
    // THIS LOAD DELAYS THE DOCUMENT'S LOAD EVENT (the set lives in core/html/browsing_context.rs) --
    // a frame's navigation and every other subresource [`delays_load`] says so of.
    // The token is taken HERE, at the one point the fetch is actually issued: every early return
    // above happens first, so an element that never loads never delays anything.
    //
    // A FRAME'S TOKEN IS THE ELEMENT ID, because the release is in `ctx_loaded`, which is handed a
    // frame element and nothing else. EVERY OTHER LOAD GETS ITS OWN token, so that two loads on one
    // element (a `<video src poster>`, an `<img>` whose `src` is rewritten in flight) cannot release
    // each other. The token travels to the completion in `pack` below; EVERY path out of this
    // function past this point releases it. A fetch that never settles holds the document's `load`
    // until the transport's own read timeout ends it -- as Chrome holds it (`when_load_delays_done`).
    // Requested AS the element's document, the load's delay token included: a frame document's `<img>` its parser
    // met was the page's request -- its Referer, its site -- and the load it delays is the frame
    // document's (a frame's `load` waits for its own loads).
    let entered = crate::core::html::browsing_context::enter_element_context(scope, id);
    let token = if framing { id } else { new_load_delay_token(scope) };
    if delays {
        delay_load(scope, token);
    }
    // The resource-timing initiator is noted only for a load that really goes to the network: a URL
    // this document already has a request for joins that request and leaves no second entry.
    //
    // A FRAMING LOAD IS NOT SHARED. Two `<iframe src=x>` are two browsing contexts and each one
    // NAVIGATES; a navigation is not a subresource and does not go through a resource cache, and the
    // completion here does not hand back a response at all -- it installs a document. The rule is
    // "every element-induced SUBRESOURCE load", and this is the line that says which are not.
    let shared = !framing;
    let style_link = tag == "link" && destination == "style";
    let preload_key = format!("{mode} {url}");
    // A stylesheet load CONSUMES a preload of the same URL and mode: it joins that response.
    let preloaded = if style_link && delays {
        let mut st = op_state.borrow_mut();
        st.try_borrow_mut::<StylePreloads>().and_then(|p| p.0.remove(&preload_key))
    } else {
        None
    };
    // A preload of what the parser already fetched is answered from that response (U16) -- or joins the parser's
    // request still in flight (the streaming parse).
    let parser_hit = if tag == "link" && !delays {
        let mut st = op_state.borrow_mut();
        st.try_borrow_mut::<ParserFetched>().and_then(|p| p.0.remove(&request_key(url, destination, mode)))
    } else {
        None
    };
    let parser_join = if tag == "link" && !delays && parser_hit.is_none() { join_parser_in_flight(scope, &request_key(url, destination, mode)) } else { None };
    // An image document's own `<img>` takes its navigation's response ([`ImageDocumentContent`]): no request,
    // so no CSP check and no resource-timing entry.
    let image_document = if tag == "img" && mode == "no-cors" { take_image_document(scope, id, url) } else { None };
    // CSP `img-src` (falling back to `default-src`) governs an image fetch: a blocked one never reaches
    // the network and fails exactly as a failed fetch does, so the element gets its `error` through
    // the same continuation. Only icons were checked; `<img>` went unchecked, which nothing showed
    // while `data:`/`blob:` images could not load at all and everything did once they could
    // (WPT content-security-policy/img-src/img-src-none-blocks-data-uri, img-src-wildcard-allowed).
    // The PAGE's policies govern the page's images, connected or not (a detached `<img>` loads too,
    // under its node document's policy); a frame document's are its own.
    //
    // EVERY DESTINATION NOW: the directive CSP3 names for the request's destination -- `style-src-elem`
    // for a stylesheet, `font-src`, `media-src`, ... -- under the element's own document's policies, a frame's too,
    // and each refusing policy reports a violation at the element (`csp::element_request_blocked`).
    let csp_blocked = image_document.is_none() && if tag == "script" {
        // `script-src` for a script a running script inserted: blocked, it never reaches
        // the network and the element gets `error` (Chrome 150, measured). Its own document's policy
        // governs it, a frame's meta included.
        !crate::core::html::csp::allows_script_url(scope, id, url, false)
    } else {
        // a frame's navigation too (`frame-src`): refused, it ends in an error page and the element's `load`, as a
        // failed navigation does (Chrome 150, measured)
        // an element `<link>` with the empty destination is a prefetch or a prerender (`destination_for`)
        let destination = if tag == "link" && destination.is_empty() { "prefetch" } else { destination };
        crate::core::html::csp::element_request_blocked(scope, id, url, destination)
    };
    // Then dangling markup, refused as CSP refuses: no request, and the element's `error`. Blink's
    // `CanRequest` checks it after CSP, so a load both refuse still reports its violation.
    let csp_blocked = csp_blocked || dangling;
    // A refused subresource load still gets its resource-timing entry, made at the refusal
    // (`resource_timing::refused_load`). A refused frame navigation is not one: it ends in an error page and gets its
    // `iframe` entry as a navigation does.
    if csp_blocked && !framing {
        crate::core::timing::resource_timing::refused_load(scope, id, tag, url);
    }
    // A stylesheet this document already has a fresh response for is Blink's memory cache's: no request, and no
    // second resource-timing entry -- `ResourceFetcher` reports a memory-cache hit only for a URL its document has
    // not loaded yet. Chrome 152.0.7977.82, 2026-10-23: a loaded `max-age=3600` link's `href` cleared and written back,
    // or toggled A -> B -> A: one entry and one request for A, two `load`s; a `no-store` one, two of each. Here the
    // HTTP cache answers it, and the entry is left out as a dirty sheet's is.
    let reused = style_link && delays && !csp_blocked && op_state.borrow().try_borrow::<LinkSheetFetches>().is_some_and(|f| f.fresh.contains(url));
    if !reused && !csp_blocked && !(shared && request_known(scope, url, destination, mode)) && preloaded.is_none() && parser_hit.is_none() && parser_join.is_none() && image_document.is_none() {
        crate::core::timing::resource_timing::note_initiator(scope, id, tag);
    }
    // The element's `referrerpolicy`, else its document's. A shared fetch an earlier
    // element already issued keeps that request's policy, as it keeps its response.
    let el_policy = crate::core::html::referrer_policy::element_request_policy(scope, id);
    set_pending_referrer_policy(scope, el_policy);
    // And its load priority -- the preload scanner's for an in-document load, else the element's own
    // (`load_priority`) -- and the credentials mode its `crossorigin` asks for.
    let mut shape = element_load_shape(scope, id, tag, destination);
    // A frame element's document may inherit its parent's encoding (`frame_parent_hint`).
    if mode == "navigate" {
        shape.parent = crate::core::html::browsing_context::frame_parent_hint(scope, id);
    }
    set_pending_load_shape(scope, shape);
    let fetched = match preloaded {
        _ if csp_blocked => refused_fetch(scope),
        Some(g) => Some(v8::Local::new(scope, &g)),
        None if image_document.is_some() => image_document.as_ref().and_then(|f| staged_response(scope, f)),
        None if parser_hit.is_some() => parser_hit.as_ref().and_then(|f| staged_response(scope, f)),
        None if parser_join.is_some() => parser_join,
        None if shared && reused => {
            crate::core::timing::resource_timing::from_origin_dirty_sheet(scope, true);
            let p = start_shared_fetch(scope, url, destination, mode);
            crate::core::timing::resource_timing::from_origin_dirty_sheet(scope, false);
            p
        }
        None if shared => start_shared_fetch(scope, url, destination, mode),
        None => start_fetch(scope, url, destination, mode),
    };
    // A refused frame navigation still gets its `iframe` entry, from the navigation's start to its end in the
    // error page -- Blink's fallback timing, which `HTMLFrameOwnerElement::DispatchLoad` adds before the frame's `load`
    // (html_frame_owner_element.cc 586-600; `resource_timing::frame_fallback_report`). Chrome 150 x3: a
    // `frame-src`-refused `<iframe>` has one, with a duration and every phase, size and status 0.
    if framing && csp_blocked {
        crate::core::timing::resource_timing::frame_fallback_start(scope, id, tag, url);
    }
    crate::core::html::browsing_context::leave_element_context(scope, entered);
    // Unconsumed when the load joined a request it did not issue.
    let _ = take_pending_referrer_policy(scope);
    let _ = take_pending_load_shape(scope);
    if let (true, false, Some(p)) = (style_link, delays, fetched) {
        let g = v8::Global::new(scope, p);
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<StylePreloads>().is_none() {
            st.put(StylePreloads::default());
        }
        st.borrow_mut::<StylePreloads>().0.insert(preload_key, g);
    }
    let Some(p) = fetched else {
        // The op is unreachable (no `Deno.core.ops` at all): the load cannot be issued, so end the
        // navigation the same way an outright failure does -- and release the token, because nothing
        // downstream will: no promise was created, so neither continuation can ever run.
        if framing {
            let null: v8::Local<v8::Value> = v8::null(scope).into();
            let u = str_val(scope, url);
            frame_navigated(scope, id, null, u);
        } else if delays {
            undelay_load(scope, token);
        }
        return;
    };
    // A stylesheet fetch for a link that is `disabled` as it starts is the parser's (a script's is never started,
    // [`maybe_load_for`]), and only such a fetch may end unheard ([`silent_sheet_end`]).
    let silent = style_link && delays && op_state.borrow().try_borrow::<HostDom>().is_some_and(|h| disabled_stylesheet_link(&h.0, id));
    if style_link && delays {
        link_sheet_fetch_started(&op_state, id, url);
    }
    let data = pack(scope, id, framing, tag == "script", destination == "style", delays, token, url, silent);
    let ok = build_fn_data(scope, cb_load_ok, "", 1, data);
    let err = build_fn_data(scope, cb_load_err, "", 1, data);
    // the fetch's end is a task (networking task source), not a promise job (`then_task`). A script's runs as the
    // response's own (its execution is a task queued from it: Chrome 152 runs an inserted async script, and fires its
    // `load`, after ONE more task of a 5 ms `MessageChannel` loop has started past the response's end, 15 of 15). Any
    // other element's `load` or `error` is a task Chrome posts once the response is processed (`then_new_task`).
    if tag == "script" {
        crate::then_task(scope, p, ok, Some(err));
    } else {
        crate::then_new_task(scope, p, ok, Some(err));
    }
}

/// Fetches refused before they reach the network (CSP), each rejected on a LATER task: a refused fetch
/// fails asynchronously like any failed fetch. Rejected at once, it fired the element's `error` at the
/// first microtask checkpoint -- before the page's first script had run, so a handler naming a
/// variable that script defines threw (WPT content-security-policy/img-src/img-src-none-blocks-data-uri).
#[derive(Default)]
struct RefusedFetches {
    next: u64,
    open: HashMap<u64, v8::Global<v8::PromiseResolver>>,
}

fn refused_fetch<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Promise>> {
    let r = v8::PromiseResolver::new(scope)?;
    let p = r.get_promise(scope);
    let g = v8::Global::new(scope, r);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let key = {
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<RefusedFetches>().is_none() {
            st.put(RefusedFetches::default());
        }
        let m = st.borrow_mut::<RefusedFetches>();
        m.next += 1;
        m.open.insert(m.next, g);
        m.next
    };
    crate::core::intersection_observer::rendering::queue_task_with_id(scope, cb_refuse_fetch, key, 0.0);
    Some(p)
}

fn cb_refuse_fetch(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(key) = crate::binding::protos::body_data(scope, &args).number_value(scope).map(|n| n as u64) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let r = op_state.borrow_mut().try_borrow_mut::<RefusedFetches>().and_then(|m| m.open.remove(&key));
    let Some(r) = r else { return };
    let r = v8::Local::new(scope, &r);
    let msg = v8::String::new(scope, "Refused to load the image because it violates the Content Security Policy.").unwrap_or_else(|| v8::String::empty(scope));
    let e = v8::Exception::type_error(scope, msg);
    r.reject(scope, e);
}

// ---- script-inserted classic scripts: "execute in order as soon as possible" ------
//
// A `<script src>` a SCRIPT inserted is either async -- it runs when it arrives -- or, when its
// `async` IDL attribute was set to false before it was prepared, it joins its document's list of
// scripts that execute IN INSERTION ORDER as soon as possible. Measured on Chrome 150.0.7871.46,
// each script announcing itself with an image named by its run order, the first served 600 ms late:
//
// ```text
// two inserted scripts, async = false                 ran-1-slow-o1, ran-2-o2
// the same, then scripts[0].async = true after insert ran-1-slow-a1, ran-2-a2   (prepared in order)
// two inserted scripts, async untouched (force-async) ran-1-f2, ran-2-slow-f1   (as they arrive)
// async = false, inserted from `onload`               ran-1-slow-m1, ran-2-m2
// ```
//
// This engine ran every inserted script as it arrived, so the in-order case was a race on the wire:
// `execution-timing/078.html` and `105.html` passed or failed with the connection's timing.

/// A script's settled fetch, waiting for its turn when it is in the in-order list.
struct ScriptDone {
    ok: bool,
    body: String,
    final_url: String,
    delay: Option<u32>,
}

// ---- the task source a settled `<script src>` executes from -----------------------
//
// THE DEFECT THIS QUEUE EXISTS FOR. HTML executes a fetched script from a TASK -- "queue an element
// task on the networking task source given el to run these steps: execute the script element". This
// engine reached it from the fetch promise's CONTINUATION instead, i.e. from inside deno_core's own
// microtask drain (`jsruntime.rs`: resolve the async ops, then `perform_microtask_checkpoint`), and
// V8 makes `PerformCheckpoint` a no-op while microtasks are running -- correctly, because a
// checkpoint inside a drain is not a checkpoint. So "clean up after running script" did
// nothing for this one shape and the script's own microtasks ran AFTER `document.currentScript` had
// been put back. Measured, Chrome 150.0.7871.46 vs korvus, one `<script src>` appended from script,
// the body reading `document.currentScript` twice (loopback, 2026-09-19):
//
// ```text
//                    while it runs   in the microtask it queued
//   chrome 150       the element     the element
//   korvus before    the element     null
//   korvus after     the element     the element
// ```
//
// THE FIX IS THE TASK SOURCE AND NOT THE CHECKPOINT. A settled script is parked here and run by the
// HOST, from Rust, between turns of the event loop (`Engine::drain_load_tasks`) -- which is the one
// place in this engine where the JavaScript execution context stack is genuinely empty, and therefore
// the one place a checkpoint is a checkpoint. It is the same shape the deferred module queue already
// has (`PendingModules`), for the same reason: two producers, one host-driven consumer.

/// Every `<script src>` whose fetch has settled and whose task has not run yet, in arrival order.
#[derive(Default)]
pub struct SettledScripts(Vec<(u32, ScriptDone)>);

/// Park a settled script's task. Called from the fetch completion; never runs the script itself.
///
/// ...and queue the task that runs it. The host drained the queue only between ITS turns of the
/// event loop -- `Engine::run`, `pump` -- so a script inserted while a CDP `Runtime.evaluate` awaited its promise,
/// or while `eval_value_settled` did, waited for that await to end: its `load` never came in time (a script's
/// `load` listeners were silent in `microtask_checkpoints_live`). The task runs the queue from Rust with nothing on
/// the stack, as the host's drain does, under any driver; the host's drain finds it empty.
fn queue_settled_script(scope: &mut v8::PinScope, id: u32, done: ScriptDone) {
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if st.try_borrow::<SettledScripts>().is_none() {
            st.put(SettledScripts::default());
        }
        st.borrow_mut::<SettledScripts>().0.push((id, done));
    }
    crate::core::html::task_queue::queue_host_step(scope, run_settled_scripts);
}

/// Is a settled script's task still waiting? The host asks so that `Engine::run` does not return with
/// the page's next script parked (it would run a turn later, which is a turn the caller may not take).
pub fn settled_scripts_pending(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>) -> bool {
    op_state.borrow().try_borrow::<SettledScripts>().is_some_and(|s| !s.0.is_empty())
}

/// **Run the parked script tasks.** The host's entry point, and the whole reason the queue exists: it
/// is called from Rust with nothing of the page on the stack, so each script's "clean up after running
/// script" performs a real microtask checkpoint while that script is still `document.currentScript`.
///
/// Drains to exhaustion in arrival order. A script that runs may settle nothing new synchronously (a
/// fetch it starts is an op, resolved by the event loop), so this terminates; the `while` is for the
/// in-order list, whose parked entries [`script_settled`] releases one after another.
pub fn run_settled_scripts(scope: &mut v8::PinScope) {
    loop {
        let next = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let mut st = op_state.borrow_mut();
            match st.try_borrow_mut::<SettledScripts>() {
                Some(q) if !q.0.is_empty() => Some(q.0.remove(0)),
                _ => None,
            }
        };
        let Some((id, done)) = next else { return };
        script_settled(scope, id, done);
    }
}

/// Per document (arena root): the in-order list, and the completions that arrived ahead of their turn.
#[derive(Default)]
struct InOrderScripts {
    lists: HashMap<u32, std::collections::VecDeque<u32>>,
    settled: HashMap<u32, ScriptDone>,
}

fn enlist_if_in_order(scope: &mut v8::PinScope, id: u32) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (has_async_attr, root) = {
        let st = op_state.borrow();
        let Some(host) = st.try_borrow::<HostDom>() else { return };
        (host.0.get_attribute(id, "async").is_some(), host.0.shadow_including_root(id))
    };
    if has_async_attr {
        return;
    }
    let el = wrap_cached(scope, &op_state, id);
    if crate::core::html::html_script_element::force_async(scope, el) {
        return;
    }
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<InOrderScripts>().is_none() {
        st.put(InOrderScripts::default());
    }
    st.borrow_mut::<InOrderScripts>().lists.entry(root).or_default().push_back(id);
}

/// A script's fetch settled: run it now, or park it until the scripts inserted before it have run.
fn script_settled(scope: &mut v8::PinScope, id: u32, done: ScriptDone) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let root = {
        let mut st = op_state.borrow_mut();
        let Some(o) = st.try_borrow_mut::<InOrderScripts>() else {
            drop(st);
            return finish_script(scope, id, done);
        };
        let Some(root) = o.lists.iter().find(|(_, q)| q.contains(&id)).map(|(r, _)| *r) else {
            drop(st);
            return finish_script(scope, id, done);
        };
        o.settled.insert(id, done);
        root
    };
    loop {
        let next = {
            let mut st = op_state.borrow_mut();
            let Some(o) = st.try_borrow_mut::<InOrderScripts>() else { return };
            let Some(front) = o.lists.get(&root).and_then(|q| q.front().copied()) else { return };
            let Some(done) = o.settled.remove(&front) else { return };
            if let Some(q) = o.lists.get_mut(&root) {
                q.pop_front();
            }
            (front, done)
        };
        finish_script(scope, next.0, next.1);
    }
}

/// Run a settled script (when it loaded), fire its `load`/`error`, release its load-delay token.
fn finish_script(scope: &mut v8::PinScope, id: u32, done: ScriptDone) {
    // A MODULE IS NOT COMPILED HERE. It goes to the deferred module queue with its
    // element and its load-delay token, and the drain runs it, fires the element's `load`/`error` and
    // releases the token -- so a module's event fires after the module RAN, as Chrome's does, and the
    // document's `load` waits for it. The queue keeps the order this function was reached in, which is
    // arrival order for a force-async script and insertion order for one whose `async` was set false
    // (`script_settled` above) -- both measured on Chrome 150.
    //
    // A NESTED context's document keeps the path it had: this engine's module map is the page realm's,
    // and evaluating a frame's module there would give it the wrong `document`. That whole realm's
    // `type=module` handling is one defect (`browsing_context.rs`'s `CLASSIC_TYPES` lists "module"),
    // not this one.
    let module = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().is_some_and(|h| crate::core::html::html_script_element::is_module_element(&h.0, id))
    };
    if module && !matches!(home_of(scope, id), Home::Frame(_)) {
        crate::core::html::html_script_element::queue_inserted_module(scope, id, done.ok, done.body, done.final_url, done.delay);
        return;
    }
    if done.ok {
        // A script that threw still LOADED: the throw is reported as the page error it is, and the
        // element's `load` below fires regardless.
        //
        // A script in a NESTED context's document runs in THAT context's realm:
        // measured, `s.src = "ifr.js"` appended to an initial `about:blank` frame's head is fetched
        // and run by Chrome.
        match home_of(scope, id) {
            Home::Frame(ctx) => crate::core::html::browsing_context::run_frame_script(scope, ctx, &done.body, &done.final_url, Some(id)),
            _ => run_classic_script(scope, id, &done.body, &done.final_url),
        }
    }
    fire(scope, id, if done.ok { "load" } else { "error" });
    if let Some(t) = done.delay {
        undelay_load(scope, t);
    }
}

/// `[nodeId, flags, url, token]` -- everything a completion needs, as the ONE value `build_fn_data`
/// carries. Bit 1 = the element is a frame, bit 2 = it is a classic script, bit 4 = the load took a
/// load-delay token (`token`) that the completion must release, bit 8 = its destination is `style`,
/// so the response body is a STYLESHEET and `cb_load_ok` must parse it.
fn pack<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    id: u32,
    framing: bool,
    script: bool,
    style: bool,
    delays: bool,
    token: u32,
    url: &str,
    silent: bool,
) -> v8::Local<'s, v8::Value> {
    let arr = v8::Array::new(scope, 4);
    let idv = v8::Integer::new_from_unsigned(scope, id);
    arr.set_index(scope, 0, idv.into());
    let flags = v8::Integer::new_from_unsigned(
        scope,
        u32::from(framing) | (u32::from(script) << 1) | (u32::from(delays) << 2) | (u32::from(style) << 3) | (u32::from(silent) << 4),
    );
    arr.set_index(scope, 1, flags.into());
    let u = str_val(scope, url);
    arr.set_index(scope, 2, u);
    let tk = v8::Integer::new_from_unsigned(scope, token);
    arr.set_index(scope, 3, tk.into());
    arr.into()
}
struct Completion {
    id: u32,
    framing: bool,
    script: bool,
    /// The element is a `<link rel=stylesheet>`: its bytes are CSS the cascade and the CSSOM both
    /// want, not a byte count to throw away.
    style: bool,
    /// The load-delay token, or `None` when this load took none ([`delays_load`]).
    delay: Option<u32>,
    url: String,
    /// A parsed `disabled` stylesheet link's fetch, which ends unheard while the link stays disabled
    /// ([`silent_sheet_end`]).
    silent: bool,
}
fn unpack(scope: &mut v8::PinScope, data: v8::Local<v8::Value>) -> Option<Completion> {
    let arr = v8::Local::<v8::Array>::try_from(data).ok()?;
    let id = arr.get_index(scope, 0)?.uint32_value(scope)?;
    let flags = arr.get_index(scope, 1)?.uint32_value(scope)?;
    let url = arr.get_index(scope, 2)?.to_rust_string_lossy(scope);
    let token = arr.get_index(scope, 3)?.uint32_value(scope)?;
    Some(Completion {
        id,
        framing: flags & 1 != 0,
        script: flags & 2 != 0,
        delay: (flags & 4 != 0).then_some(token),
        style: flags & 8 != 0,
        url,
        silent: flags & 16 != 0,
    })
}

/// This document's subresource requests, by `"<destination> <mode> <url>"`: the fetch's promise.
///
/// A LOAD JOINS THE REQUEST THIS DOCUMENT ALREADY HAS FOR THE SAME URL. Blink's memory cache is one
/// mechanism with two halves, and only the first is here:
///
/// * **In flight -- ALWAYS, whatever the response will say.** Measured on stock Chrome
///   150.0.7871.46 over a loopback server answering `Cache-Control: no-store`, so nothing could be
///   reused from any cache: three `<link rel=stylesheet>` naming one file, three `<link
///   rel=prefetch>`, five `<script src>` a script appended in one loop, and eight concurrent
///   `import()` of one module -- **every one of them is ONE request**. This engine made 3, 3, 5
///   and 8, and makes 1 of each now.
/// * **Settled -- header-dependent, and NOT implemented.** The same fixtures with a 300 ms gap
///   instead of a burst: Chrome re-requests a `no-store` script (2 for a parser script plus a later
///   inserted one) and does NOT re-request it under `Cache-Control: max-age=300` (1). That half is
///   HTTP freshness and this engine has no HTTP cache (`korvus-transport`: "this engine has no HTTP
///   cache, so every row is a network hit"), so an entry is dropped the moment its fetch settles --
///   except for an image, below, where the retention is measured rather than assumed.
///
/// WHAT THIS MAP DOES NOT REACH, said here rather than discovered later: a `<script src>` THE
/// PARSER found. `parser_load_is_owned_elsewhere` sends those to `Engine::load_page`, which fetches
/// them itself and joins nothing -- so three `<script async src=x>` in markup are still three
/// requests here where Chrome makes one, measured on the same `no-store` server. Three
/// parser-BLOCKING `<script src=x>` are three in Chrome too, because they are sequential and the
/// response may not be reused, so the gap is exactly the burst the parser issues at once.
///
/// The key carries the DESTINATION as well as the URL and mode, because they are different requests:
/// `Sec-Fetch-Dest` differs on the wire, and a server may answer them differently.
///
/// AN IMAGE'S ENTRY IS RETAINED, and that is a measurement, not an optimisation. Chrome
/// 150.0.7871.46 over the same `no-store` server: two `<img src=a.png>`, a third `new Image()`
/// pointed at it 300 ms later, an `<img>` and a CSS `background: url(a.png)`, a `srcset` candidate
/// and a `src` naming one file -- each is ONE request, where the same shape with a `<script>` is
/// two. Its body is dropped because nothing reads an image's bytes (R7) and a page with a thousand
/// images would otherwise keep every one of them alive for the life of the document.
///
/// A FONT'S ENTRY IS RETAINED TOO, also without its body: a face's bytes are parsed where they arrive
/// (`font_face::font_parsed`), and every reader of a font's response takes its status, URL and headers. Blink makes
/// every CSS-connected face anew when a sheet's rules move (`ClearFontFaceCacheAndAddUserFonts`, here
/// `font_face::css_faces`) -- an `@import` arriving does it -- and the new faces come from the same
/// `StyleRuleFontFace`, whose `CSSFontFaceSrcValue` keeps the `FontResource` it fetched; `ResourceFetcher` would
/// reuse it during the initial load anyway (`resource_fetcher.cc`: "Avoid making multiple requests for the same URL
/// during the initial load"). Forgotten on settle, a rebuilt face asked the network again whenever the first
/// response had already arrived: `stylesheet_loads_live`'s `@import` page requested `used.woff2` twice in 1 of 10
/// runs, and in 6 of 10 once the timing moved, where Chrome 150 requests it once.
///
/// Each element still gets its own `load`/`error`, off the same settled promise.
#[derive(Default)]
struct DocumentRequests(HashMap<String, v8::Global<v8::Promise>>);

fn request_key(url: &str, destination: &str, mode: &str) -> String {
    format!("{destination} {mode} {url}")
}

/// THE PRELOAD SCANNER OVER HELD-BACK WRITTEN INPUT. A parser-blocking `<script src>` that
/// `document.write` wrote holds back the input written after it ([`crate::core::dom::document`]'s `WrittenInput`),
/// and korvus inserts those nodes, and so requests what they load, only once the script has run. Chrome's preload
/// scanner reads that input while the script is still being fetched and starts its loads then (Blink
/// `HTMLDocumentParser`'s preload scanner over `document.write` input). WPT
/// `html/syntax/speculative-parsing/generated/document-write/link-rel-preload-as-font-crossorigin` writes a 1.5 s
/// script and then a font preload. Chrome passes it; here the preload left only just before `load`, and lost the race
/// against the test's own read of the server's record ("speculative case did not fetch", most runs).
///
/// Scanned here, as Rust data off the held-back nodes: an `<img src>` (not `loading=lazy`) and a
/// `<link rel=preload as=image|font>`, in tree order. Each is requested now with its element's own request shape
/// (initiator, referrer policy, priority). The element joins that request when it is inserted, because
/// [`DocumentRequests`] keeps an image's or a font's for the life of the document. A script or a stylesheet is not
/// scanned: its request is forgotten when it settles, so a speculated one could be asked for twice.
pub(crate) fn speculate_written(scope: &mut v8::PinScope, nodes: &[u32]) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let wanted: Vec<(u32, &'static str, String, &'static str, &'static str)> = {
        let st = op_state.borrow();
        let media = crate::core::dom::geometry_queries::media_context(&st);
        let Some(host) = st.try_borrow::<HostDom>() else { return };
        let dom = &host.0;
        let mut out = Vec::new();
        let mut stack: Vec<u32> = nodes.iter().rev().copied().collect();
        while let Some(n) = stack.pop() {
            let mut kids = dom.children(n);
            kids.reverse();
            stack.extend(kids);
            let Some(tag) = dom.tag_name(n).map(|t| t.to_ascii_lowercase()) else { continue };
            // A `<meta http-equiv=Content-Security-Policy>` in the held-back input voids the whole scan:
            // Blink's scanner counts it (`csp_meta_tag_count`), and `HTMLDocumentParser` holds EVERY queued preload --
            // those before it in the input too -- until the parser has applied each policy it counted
            // (html_document_parser.cc, `seen_csp_meta_tags_`). The loads are then the inserted elements' own, under the
            // policy. Scanned past, `<meta ... img-src 'none'><img>` fetched the image (WPT
            // speculative-parsing/generated/document-write/meta-csp-img-src-none; Chrome 150: not fetched).
            if tag == "meta" && dom.get_attribute(n, "http-equiv").is_some_and(|v| v.trim().eq_ignore_ascii_case("content-security-policy")) {
                return;
            }
            let cors = dom.get_attribute(n, "crossorigin").is_some();
            let mode = if cors { "cors" } else { "no-cors" };
            if tag == "img" {
                if is_lazy(dom, n) {
                    continue;
                }
                if let Some(raw) = image_source(dom, n, &media) {
                    out.push((n, "img", raw, "image", mode));
                }
            } else if tag == "link" {
                let preload = dom.get_attribute(n, "rel").is_some_and(|r| r.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("preload")));
                let destination = match dom.get_attribute(n, "as").map(|a| a.trim().to_ascii_lowercase()).as_deref() {
                    Some("image") => "image",
                    Some("font") => "font",
                    _ => continue,
                };
                if let (true, Some(raw)) = (preload, dom.get_attribute(n, "href")) {
                    out.push((n, "link", raw.to_string(), destination, mode));
                }
            }
        }
        out
    };
    for (id, tag, raw, destination, mode) in wanted {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let url = resolve(&base_for_detached(scope, Home::Page), raw);
        if !(has_scheme(&url, "http") || has_scheme(&url, "https")) {
            continue;
        }
        // refused, the scanner's request still has its entry (as `preload_scan`'s)
        if dangling_http(raw, &url) || !crate::core::html::csp::page_request_allowed_quietly(scope, &url, destination) {
            crate::core::timing::resource_timing::refused_load(scope, id, tag, &url);
            continue;
        }
        if request_known(scope, &url, destination, mode) {
            continue;
        }
        crate::core::timing::resource_timing::note_initiator(scope, id, tag);
        let policy = crate::core::html::referrer_policy::element_request_policy(scope, id);
        set_pending_referrer_policy(scope, policy);
        let shape = element_load_shape(scope, id, tag, destination);
        set_pending_load_shape(scope, shape);
        let _ = start_shared_fetch(scope, &url, destination, mode);
        let _ = take_pending_referrer_policy(scope);
        let _ = take_pending_load_shape(scope);
    }
}

/// What [`DocumentRequests`] keeps a shared request under: its URL, destination and mode, and -- for a font -- the
/// document it is made for. An interning change kept a font's response for the life of the document so a second
/// `@font-face` or preload of the URL joins the first request, as Blink's per-document `ResourceFetcher` does. But
/// the table is the process's, and a FRAME document's font preload of a URL the page had already asked for joined
/// the page's request and was never made. Across documents Blink's memory cache serves only a fresh response (WPT
/// speculative-parsing `link-rel-preload-as-font-crossorigin`: two documents, two requests, which Chrome 150 passes).
/// Images keep sharing one request across documents as they always have here.
fn shared_key(scope: &mut v8::PinScope, url: &str, destination: &str, mode: &str) -> String {
    let key = request_key(url, destination, mode);
    match (destination, crate::core::html::browsing_context::running_frame(scope)) {
        ("font", Some(frame)) => format!("{key} #frame {frame}"),
        _ => key,
    }
}

/// Has this document already got a request for `url` that this load would join?
fn request_known(scope: &mut v8::PinScope, url: &str, destination: &str, mode: &str) -> bool {
    let key = shared_key(scope, url, destination, mode);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<DocumentRequests>().is_some_and(|r| r.0.contains_key(&key))
}

/// Fetch `url` for this document, or join the request it already has for it. The CSS loader
/// (`core/css/sheet_loads.rs`) comes through here too, so an `<img>` and a background naming one
/// URL share one request, and so do two `@font-face` rules. See [`DocumentRequests`] for what
/// "already has" means for an image and for everything else.
pub(crate) fn start_shared_fetch<'s>(scope: &mut v8::PinScope<'s, '_>, url: &str, destination: &'static str, mode: &str) -> Option<v8::Local<'s, v8::Promise>> {
    let key = shared_key(scope, url, destination, mode);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let known = op_state.borrow().try_borrow::<DocumentRequests>().and_then(|r| r.0.get(&key).cloned());
    if let Some(g) = known {
        return Some(v8::Local::new(scope, &g));
    }
    let p = start_fetch(scope, url, destination, mode)?;
    // An image's or a font's response is kept without its body, for the life of the document; every other
    // destination's is kept whole, because the caller reads it -- a script runs it, a stylesheet parses it -- and
    // is forgotten on settle.
    let shared = if destination == "image" || destination == "font" {
        let strip = crate::binding::protos::build_fn(scope, cb_strip_body, "", 1);
        p.then(scope, strip)?
    } else {
        let k = str_val(scope, &key);
        let forget = build_fn_data(scope, cb_forget_request, "", 1, k);
        p.then2(scope, forget, forget);
        p
    };
    let g = v8::Global::new(scope, shared);
    let mut st = op_state.borrow_mut();
    if st.try_borrow::<DocumentRequests>().is_none() {
        st.put(DocumentRequests::default());
    }
    st.borrow_mut::<DocumentRequests>().0.insert(key, g);
    Some(shared)
}

/// The settled half of [`DocumentRequests`]: a request that is no longer in flight is no longer
/// joinable, because this engine has no HTTP cache to decide whether its response may be reused.
fn cb_forget_request(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let key = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        if let Some(r) = st.try_borrow_mut::<DocumentRequests>() {
            r.0.remove(&key);
        }
    }
    rv.set(args.get(0));
}

/// The response without its body: `{status, url, headers}`.
fn cb_strip_body(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Ok(resp) = v8::Local::<v8::Object>::try_from(args.get(0)) else {
        rv.set(args.get(0));
        return;
    };
    let out = v8::Object::new(scope);
    for k in ["status", "url", "headers"] {
        if let Some(v) = get_prop(scope, resp, k) {
            crate::binding::protos::set_prop(scope, out, k, v);
        }
    }
    rv.set(out.into());
}

/// `op_subresource_fetch(url, destination, mode)` as a promise. The op is reached through the stashed
/// `Deno.core.ops` object (`op_stash_ops`, before the `Deno` global is deleted for stealth), the same
/// way XHR reaches `op_fetch`: it is the engine's own loader, never on the page's surface.
pub(crate) fn start_fetch<'s>(scope: &mut v8::PinScope<'s, '_>, url: &str, dest: &str, mode: &str) -> Option<v8::Local<'s, v8::Promise>> {
    start_fetch_with(scope, url, dest, mode, "GET", None, None)
}

/// A NESTED context's own navigation, with the method and body a `method=post` form submission
/// carries. Destination `document`, mode `navigate`: the request a browser issues for a browsing
/// context's document, and the only one that carries `Sec-Fetch-Dest: document`.
pub(crate) fn start_navigation_fetch<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    url: &str,
    dest: &str,
    method: &str,
    body: Option<&str>,
    content_type: Option<&str>,
) -> Option<v8::Local<'s, v8::Promise>> {
    start_fetch_with(scope, url, dest, "navigate", method, body, content_type)
}

/// The referrer policy the NEXT load this realm issues goes out under, when its caller knows better than
/// the running document (an element's `referrerpolicy`, a frame's embedder): set right before the call,
/// taken by [`start_fetch_with`].
#[derive(Default)]
struct PendingReferrerPolicy(Option<Option<transport::ReferrerPolicy>>);

pub(crate) fn set_pending_referrer_policy(scope: &mut v8::PinScope, p: Option<transport::ReferrerPolicy>) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().put(PendingReferrerPolicy(Some(p)));
}

pub(crate) fn take_pending_referrer_policy(scope: &mut v8::PinScope) -> Option<Option<transport::ReferrerPolicy>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    st.try_borrow_mut::<PendingReferrerPolicy>().and_then(|p| p.0.take())
}

/// What the NEXT load this realm issues states about itself beyond its URL and mode:
/// the `priority` field Blink's load priority gives it (`load_priority`; `None` is the destination's
/// default) and the credentials mode its element asked for (`"include"` for `crossorigin=use-credentials`,
/// `""` for the mode's own). Set right before the call, taken by [`start_fetch_with`], like the referrer
/// policy above.
#[derive(Default, Clone)]
pub(crate) struct LoadShape {
    pub(crate) priority: Option<String>,
    pub(crate) credentials: &'static str,
    /// A frame navigation's parent document `(encoding, origin)` (`browsing_context::frame_parent_hint`),
    /// for the decoder's "parent's encoding" step. `None` for every other load.
    pub(crate) parent: Option<(String, String)>,
}

#[derive(Default)]
struct PendingLoadShape(Option<LoadShape>);

pub(crate) fn set_pending_load_shape(scope: &mut v8::PinScope, shape: LoadShape) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    op_state.borrow_mut().put(PendingLoadShape(Some(shape)));
}

pub(crate) fn take_pending_load_shape(scope: &mut v8::PinScope) -> Option<LoadShape> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    st.try_borrow_mut::<PendingLoadShape>().and_then(|p| p.0.take())
}

fn start_fetch_with<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    url: &str,
    dest: &str,
    mode: &str,
    method: &str,
    body: Option<&str>,
    content_type: Option<&str>,
) -> Option<v8::Local<'s, v8::Promise>> {
    // The referrer policy: what the caller set for this one load, else the running document's (ACCESS
    // 2026-10-04). Taken first, so an early return cannot leave it for an unrelated later load.
    let policy = match take_pending_referrer_policy(scope) {
        Some(p) => p,
        None => crate::core::html::referrer_policy::running_policy(scope),
    };
    let shape = take_pending_load_shape(scope).unwrap_or_default();
    let f = deno_op(scope, "op_subresource_fetch")?;
    // A FRAME's element loads are the frame document's requests: resolved against ITS base,
    // initiated -- Referer, Sec-Fetch-Site -- by ITS URL. They were the top document's: a frame's
    // `img.src = 'p.png'` went to the TOP page's origin, with the top's Referer (Brave 151 and Chrome 150:
    // the frame's origin and URL). `""` is the page's own document, which the loader already knows.
    // A frame in an opaque origin (a sandboxed document) names its client marked as such: its loads carry no
    // `Referer`, a CORS one `Origin: null`, and all are cross-site (Chrome 150 loads a sandboxed
    // srcdoc frame's `<script src>` with no Referer -- a bot-detection script's `ri.px-cloud.net/index.js` -- where korvus sent
    // the page's).
    let (client, resolved) = match crate::core::html::browsing_context::running_frame(scope) {
        Some(id) => {
            let (_, base) = crate::core::html::browsing_context::frame_request_client(scope, id);
            let client = crate::core::html::browsing_context::request_client(scope, id);
            let resolved = url::Url::parse(&base).ok().and_then(|b| b.join(url).ok()).map(|u| u.to_string());
            (client, resolved.unwrap_or_else(|| url.to_string()))
        }
        None => (String::new(), url.to_string()),
    };
    let url = resolved.as_str();
    let u = str_val(scope, url);
    let d = str_val(scope, dest);
    let m = str_val(scope, mode);
    let meth = str_val(scope, method);
    let b: v8::Local<v8::Value> = match body {
        Some(s) => str_val(scope, s),
        None => v8::null(scope).into(),
    };
    let ct: v8::Local<v8::Value> = match content_type {
        Some(s) => str_val(scope, s),
        None => v8::null(scope).into(),
    };
    // The requesting document's site for cookies.
    let sfc = crate::core::html::browsing_context::realm_site_for_cookies(scope);
    let sfc = str_val(scope, &sfc);
    let cl = str_val(scope, &client);
    let rp = str_val(scope, crate::core::html::referrer_policy::token(policy));
    let who = v8::Object::new(scope);
    crate::binding::protos::set_prop(scope, who, "siteForCookies", sfc);
    crate::binding::protos::set_prop(scope, who, "clientUrl", cl);
    crate::binding::protos::set_prop(scope, who, "referrerPolicy", rp);
    if let Some(p) = &shape.priority {
        let pv = str_val(scope, p);
        crate::binding::protos::set_prop(scope, who, "priority", pv);
    }
    if !shape.credentials.is_empty() {
        let cv = str_val(scope, shape.credentials);
        crate::binding::protos::set_prop(scope, who, "credentials", cv);
    }
    if let (Some((enc, origin)), "navigate") = (&shape.parent, mode) {
        let ev = str_val(scope, enc);
        crate::binding::protos::set_prop(scope, who, "parentEncoding", ev);
        let ov = str_val(scope, origin);
        crate::binding::protos::set_prop(scope, who, "parentOrigin", ov);
    }
    // A NAVIGATION STARTS NOW, when it is initiated (HTML: its document's time origin), before its request is put
    // together. Chrome 150: a frame's `performance.timeOrigin` is 0.5-0.9 ms after its element
    // was inserted; korvus's was 14-29 ms after, at the request it sent on the event loop's next turn, and its
    // navigation entry's `fetchStart` 0.
    if mode == "navigate" {
        let at = crate::core::html::browsing_context::navigation_begins_now(scope);
        let at = v8::Number::new(scope, at);
        crate::binding::protos::set_prop(scope, who, "navigationStart", at.into());
    }
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let g = {
        v8::tc_scope!(let tc, scope);
        let r = f
            .call(tc, undef, &[u, d, m, meth, b, ct, who.into()])
            .and_then(|p| v8::Local::<v8::Promise>::try_from(p).ok())
            .map(|p| v8::Global::new(tc, p));
        let caught = tc.has_caught();
        tc.reset();
        if caught {
            None
        } else {
            r
        }
    }?;
    let p = v8::Local::new(scope, &g);
    crate::core::timing::resource_timing::watch_load(scope, p, url, dest, mode);
    Some(p)
}

/// Is the loader reachable at all? A context whose `Deno.core.ops` was never stashed can issue no
/// load, which is what makes `window.open` answer `null` there.
pub(crate) fn loader_ready(scope: &mut v8::PinScope) -> bool {
    deno_op(scope, "op_subresource_fetch").is_some()
}

/// A classic script's source is run in the global scope, like the parser-inserted ones in
/// RUN_PAGE_SCRIPTS: a top-level compile+run, which is what the deleted indirect `eval` was -- so
/// `var`/`function` declarations land on globalThis.
///
/// TWO THINGS A RUNNING SCRIPT SEES, both measured on Chrome 150.0.7871.46 for a `<script src>`
/// inserted from script:
///
/// ```text
/// document.currentScript inside the body     the element            (korvus: null)
/// document.currentScript in its onload        null
/// a body that throws TypeError('boom')        ErrorEvent at window, message "Uncaught TypeError: boom",
///                                             an uncaught exception, then `load` at the element
///                                             (korvus: a bare Event "error" at window, no message,
///                                             the exception discarded)
/// ```
///
/// A bundler that finds its own chunk URL through `document.currentScript` read null here and threw.
fn run_classic_script(scope: &mut v8::PinScope, id: u32, code: &str, url: &str) {
    let Some(src) = v8::String::new(scope, code) else { return };
    // The script's URL is its ORIGIN, as for a parser-inserted script (`korvus-runtime`'s
    // `run_classic`): a `//# sourceURL=` comment appended to the body left V8's stack frames reading
    // `<anonymous>` where Chrome names the script's URL.
    let Some(name) = v8::String::new(scope, url.split('\n').next().unwrap_or("")) else { return };
    let origin = v8::ScriptOrigin::new(scope, name.into(), 0, 0, false, 0, None, false, false, false, None);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let el = wrap_cached(scope, &op_state, id);
    // Saved and restored, not cleared: a script inserted and run from inside another script nests.
    let prev = current_script(scope).map(|v| v8::Global::new(scope, v));
    let cs = crate::core::html::html_script_element::current_script_value(scope, el.into());
    set_current_script(scope, cs);
    let thrown = {
        v8::tc_scope!(let tc, scope);
        let _ = v8::Script::compile(tc, src, Some(&origin)).and_then(|s| s.run(tc));
        // reported where it was thrown (`report::caught_site`)
        let t = if tc.has_caught() {
            tc.exception().map(|e| {
                let m = tc.message();
                (v8::Global::new(tc, e), crate::core::events::report::caught_site(tc, e, m))
            })
        } else {
            None
        };
        tc.reset();
        t
    };
    if let Some((t, site)) = thrown {
        let t = v8::Local::new(scope, &t);
        crate::core::events::report::report_exception_at(scope, t, site);
    }
    // THE REPORT MOVED ABOVE THE RESTORE, AND THAT PART IS MEASURED AND LIVE. HTML reports the
    // exception inside "run a classic script" and restores `currentScript` only after it returns, so
    // a `window.onerror` handler sees the throwing script. Chrome 150.0.7871.46, a script that throws,
    // `document.currentScript` read from the window's `error` listener (loopback,
    // 2026-09-19):
    //
    // ```text
    //                       parser-inserted   inserted from script
    //   chrome 150          the element       the element
    //   korvus before       the element       null            <- this reorder
    //   korvus after        the element       the element
    // ```
    //
    // HTML "clean up after running script" follows it, INSIDE the save/restore pair on purpose: the
    // microtasks this script queued must see it as `document.currentScript`, as they do in Chrome
    // (the table in `html_script_element::clean_up_after_running_script`).
    //
    // AND IT IS INERT TODAY, WHICH IS A DEFECT SOMEWHERE ELSE, NAMED HERE SO NOBODY READS THIS LINE
    // AS THE BEHAVIOUR. HTML executes a fetched script from a TASK ("queue a task to execute the
    // script element"); this engine reaches it from the fetch promise's continuation, i.e. from
    // inside deno_core's own microtask checkpoint (`jsruntime.rs`: resolve the async ops, then
    // `perform_microtask_checkpoint`). V8 makes `PerformCheckpoint` a no-op while microtasks are
    // running, so the checkpoint below does nothing and this script's continuation runs after the
    // restore. Measured, Chrome 150.0.7871.46 vs korvus, one `<script src>` appended from script,
    // the body reading `document.currentScript` twice (loopback, 2026-09-19):
    //
    // ```text
    //                    while it runs   in the microtask it queued
    //   chrome 150       the element     the element
    //   korvus           the element     null            <- this line cannot fix it
    // ```
    //
    // The fix is to execute a settled script from a task rather than from the promise continuation,
    // which is the load pipeline's task source and not `document.currentScript`; when that lands
    // this line starts doing its job with no further change. The parser-inserted shapes -- inline,
    // external, `async`, `defer` -- are all driven from Rust with an empty JS stack, so they are not
    // affected and are covered by `tests/current_script_checkpoint_live.rs`.
    crate::core::html::html_script_element::clean_up_after_running_script(scope);
    let restore: v8::Local<v8::Value> = match prev {
        Some(p) => v8::Local::new(scope, &p),
        None => v8::undefined(scope).into(),
    };
    set_current_script(scope, restore);
}

/// `dispatchEvent(new Event(type))` at the element. An element torn down mid-flight has nothing to
/// fire at, which `wrap_cached` handles by minting a fresh wrapper over the (still-live) arena node.
fn fire(scope: &mut v8::PinScope, id: u32, ty: &str) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let el = wrap_cached(scope, &op_state, id);
    fire_at(scope, el.into(), ty);
}

fn cb_load_ok(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(Completion { id, framing, script, style, delay, url: raw, silent }) = unpack(scope, __body_data) else { return };
    // A stylesheet fetch for an `href` the link no longer names was cancelled when the attribute moved on
    // ([`link_fetch_current`]): no sheet and no event come of it.
    if style && delay.is_some() && !link_fetch_current(scope, id, &raw) {
        link_fetch_cancelled(scope, id, &raw);
        if let Some(t) = delay {
            undelay_load(scope, t);
        }
        return;
    }
    let resp = v8::Local::<v8::Object>::try_from(args.get(0)).ok();
    let status = resp.and_then(|r| get_prop(scope, r, "status")).and_then(|s| s.int32_value(scope)).unwrap_or(0);
    let ok = resp.is_some() && (200..300).contains(&status);
    let body = resp
        .and_then(|r| get_prop(scope, r, "body"))
        .filter(|b| !b.is_null_or_undefined())
        .map(|b| b.to_rust_string_lossy(scope))
        .unwrap_or_default();
    let final_url = resp
        .and_then(|r| get_prop(scope, r, "url"))
        .filter(|u| u.boolean_value(scope))
        .map(|u| u.to_rust_string_lossy(scope))
        .unwrap_or_else(|| raw.clone());
    // A SCRIPT HAS ITS OWN IDEA OF "OK": any status below 400, and a Content-Type the
    // classic-script MIME check does not refuse. Measured on Chrome, table in
    // `html_script_element::script_response_ok` -- a 300 or a 304 runs and fires `load` there, where
    // the 2xx test every other destination uses here would fire `error`.
    // A MODULE script's idea of "ok" is stricter still: a JavaScript MIME type, nosniff
    // or not. The flavour is the ELEMENT's, so both halves of the table are asked by one call.
    let ok = if script {
        let headers = resp.and_then(|r| get_prop(scope, r, "headers")).map(|h| headers_from_js(scope, h)).unwrap_or_default();
        let flavor = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            st.try_borrow::<HostDom>().map(|h| crate::core::html::html_script_element::element_flavor(&h.0, id)).unwrap_or(ScriptFlavor::Classic)
        };
        resp.is_some() && script_response_ok(flavor, u16::try_from(status).unwrap_or(0), &headers)
    } else {
        ok
    };
    if script {
        // PARKED, NOT RUN. This is the fetch promise's continuation, inside deno_core's
        // microtask drain; HTML executes a fetched script from a TASK, and the host runs it from one.
        // See [`SettledScripts`].
        // compiled off this thread from now on, for its run to take (`script_stream`)
        if ok {
            crate::core::html::script_stream::start(scope, &body);
        }
        queue_settled_script(scope, id, ScriptDone { ok, body, final_url, delay });
        return;
    }
    // A parsed `disabled` stylesheet link's load ends unheard ([`silent_sheet_end`]).
    let silent = style && silent && silent_sheet_end(scope, id, &raw);
    if style && !silent {
        link_sheet_ended(scope, id, &raw, delay.is_some());
    }
    if ok && !framing && !script && !style {
        let manifest = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            st.try_borrow::<HostDom>().is_some_and(|h| {
                h.0.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("link"))
                    && h.0.get_attribute(id, "rel").is_some_and(|r| r.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("manifest")))
            })
        };
        if manifest {
            crate::core::html::link_icons::manifest_arrived(scope, id, &body, &final_url);
        }
    }
    // A `style` destination is a stylesheet only while the element IS a stylesheet link: a
    // `rel=preload as=style` response is kept for a later stylesheet load to consume, never applied
    // itself (`<link rel=preload href=c.css as=style>` alone styled the page here; Chrome
    // 150 leaves `.pc` visible).
    let style = style && {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().is_some_and(|h| page::is_stylesheet_link(&h.0, id))
    };
    // A 2xx sheet fires what [`stylesheet_arrived`] says: `load`, but for one `nosniff` refuses.
    let ok = if ok && style {
        let headers = resp.and_then(|r| get_prop(scope, r, "headers")).map(|h| headers_from_js(scope, h)).unwrap_or_default();
        if fresh_for_reuse(&headers) {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            link_fetches(&mut op_state.borrow_mut()).fresh.insert(raw.clone());
        }
        stylesheet_arrived(scope, id, &body, &final_url, &headers)
    } else {
        ok
    };
    if style {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        link_sheet_outcome(&op_state, id, ok);
    }
    // A FRAME's response is a DOCUMENT, not a byte count. `frame_navigated` builds the document, runs
    // its scripts and fires `load` itself, so this path must not fire it a second time.
    if framing {
        // 204 No Content / 205 Reset Content abort the navigation: the frame keeps the document it had
        // and fires nothing at all. Treating them as a successful empty document is what
        // `initial-empty-document/iframe-src-204.html` measures.
        if status == 204 || status == 205 {
            if let Some(t) = delay {
                undelay_load(scope, t);
            }
            return;
        }
        // ANY HTTP RESPONSE IS THE FRAME'S DOCUMENT, whatever its status. Chromium commits
        // a 4xx/5xx body in a frame as the document it is -- `HttpErrorNavigationThrottle`, which swaps an EMPTY
        // error body for Chrome's own error page, is added for the primary main frame alone -- with its scripts,
        // its headers' policies and its status in the frame's Navigation Timing entry. Chrome 150, measured
        // (`fixtures/frame-status`): frames answered 403, 404, 500 and 418 with an HTML body run their scripts
        // and read `responseStatus` 403, 404, ...; an empty 404 or 500 is an empty, readable document; a 404 sent
        // as `text/plain` is a text document; every one fires `load` at its element. This engine built an empty
        // document for any status outside 2xx, so a device-check script 403 leboncoin's auth frame answers never ran.
        let response = resp.is_some();
        let delivered = if response { resp.and_then(|r| get_prop(scope, r, "headers")).map(|h| headers_from_js(scope, h)).unwrap_or_default() } else { Vec::new() };
        let timing = if response { resp.and_then(|r| crate::core::html::browsing_context::response_timing(scope, r)) } else { None };
        crate::core::html::browsing_context::frame_response_delivered(scope, id, &delivered, timing);
        // The encoding the runtime decoded the frame's document with, for its `characterSet`, its forms and
        // its links.
        let encoding = resp.and_then(|r| get_prop(scope, r, "encoding")).filter(|e| e.is_string()).map(|e| e.to_rust_string_lossy(scope));
        crate::core::html::browsing_context::frame_encoding_delivered(scope, id, encoding);
        let b: v8::Local<v8::Value> = if response { str_val(scope, &body) } else { v8::null(scope).into() };
        let u = str_val(scope, &final_url);
        frame_navigated(scope, id, b, u);
        return;
    }
    {
        let content_type = ok.then(|| {
            let headers = resp.and_then(|r| get_prop(scope, r, "headers")).map(|h| headers_from_js(scope, h)).unwrap_or_default();
            // The type the object's navigation commits as: the network stack's (A/B C5, `transport::mime`), every
            // `Content-Type` value read, the last valid one winning.
            transport::mime::mime_type_and_charset(&headers).mime
        });
        note_object_outcome(scope, id, &raw, content_type);
    }
    // AN IMAGE THAT ARRIVED HAS A SIZE THE LAYOUT MUST USE: an `<img>` laid out at 0x0
    // whatever it had loaded, because no image size ever reached the layout path.
    let is_img = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().is_some_and(|h| h.0.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("img")))
    };
    // A response whose bytes are not an image is a broken image: `error`, not `load`.
    let ok = if is_img && ok { crate::core::html::canvas_image::fetched_image_decodes(scope, id, &raw) != Some(false) } else { ok };
    // A BROKEN ONE HAS ITS FALLBACK CONTENT, and has it before its `error` handler runs (A/B C8): Blink's
    // `HTMLImageLoader::ImageNotifyFinished` resets the layout disposition first. Chrome 150 reads a
    // broken `<img>` 16x16 inside `onerror` (`broken_img_live`); it read 0x0 here, which is how
    // bot.incolumitas.com's `intoli.brokenImageDimensions` tells a headless browser.
    if is_img {
        let size = if ok { crate::core::html::canvas_image::img_layout_size(scope, id) } else { None };
        let state = if ok { size.map(|(w, h)| page::ImageState::Available(w, h)) } else { Some(page::ImageState::Broken) };
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        crate::core::dom::geometry_queries::NaturalImages::set(&mut st, id, state);
        // The request settled (available or broken): `complete` reads `true` again.
        mark_image_settled(&mut st, id);
    }
    let heard = !silent && !(style && !element_connected(scope, id));
    // A SHEET'S `load` WAITS FOR ITS `@import`s, and is `error` when one failed, as a `<style>`'s does
    // ([`sheet_load_settled`]). Chrome 152.0.7977.82, 2026-10-23: a link to a sheet importing a 404 fires `error`, one
    // importing a sheet 400 ms away fires `load` 400 ms after its own response; this engine fired `load` at its own
    // response for both (WPT `quirk-origin-check-recursive-import`: the `data:/,` sheet's `@import url('x/')` cannot
    // resolve, so its one `error`). The load-delay token is the wait's to release.
    if heard && style && ok {
        let token = match delay {
            Some(t) => t,
            None => {
                let t = new_load_delay_token(scope);
                delay_load(scope, t);
                t
            }
        };
        if !sheet_load_settled(scope, id, token) {
            crate::core::css::sheet_loads::wait_for_imports(scope, id, token);
        }
        return;
    }
    if heard {
        fire(scope, id, if ok { "load" } else { "error" });
    }
    // `img.decode()` called while the request was in flight settles after the element's own event.
    if is_img {
        crate::core::html::html_image_element::settle_decodes(scope, id, ok);
    }
    // THE RELEASE, AFTER the element's own event. A browser fires a subresource's `load` before the
    // document's -- the element handler is what a page's `load` handler is allowed to assume has
    // already run -- and `undelay_load` only schedules the document's load step on a fresh task
    // anyway, so the order here is the observable one either way.
    if let Some(t) = delay {
        undelay_load(scope, t);
    }
}

// ---- a `<link rel=stylesheet>`'s bytes --------------------------------------------
//
// THE DEFECT THIS SECTION EXISTS FOR. `cb_load_ok` read the response body and used it for exactly two
// destinations, `script` and a frame's document. A stylesheet's bytes were fetched -- L2's request
// ledger shows `/s.css` going out exactly once -- and then dropped, so no author rule from a linked
// sheet reached the cascade OR the CSSOM. Measured at `9fced3cc` on a loopback server:
//
//     requests ["/s.css"]                         the bytes arrive
//     styleSheets[0].cssRules.length  0           and are never parsed
//     getComputedStyle(#lk).width     1904px      the cascade never sees them either (the rule says 123px)
//     styleSheets[1] (an inline <style>) 2 rules  which is why only the inline half worked
//
// The third line corrects P2a's report, which had this as a CSSOM-only defect with the declarations
// "reaching the element". They do not: the sheet is not parsed at all, in either subsystem.

/// Is this response body CSS the document may parse?
///
/// Measured on stock Chrome 150.0.7871.46, a no-quirks document, one same-origin link per row:
///
/// ```text
/// Content-Type: text/css      cssRules 1   cascade 123px
/// Content-Type: text/plain    cssRules 0   cascade  (not applied)
/// no Content-Type at all      cssRules 1   cascade 123px
/// ```
///
/// That is the no-quirks rule; a quirks document takes a CORS-same-origin sheet of any type that `nosniff` lets
/// through, and a refused sheet still fires `load` ([`stylesheet_arrived`]).
///
/// The type read is Blink's `HttpContentType()` (`CSSStyleSheetResource::CanUseSheet`, `css_style_sheet_resource.cc:220`;
/// the `nosniff` check `ResourceLoader::CheckResponseNosniff` reads the same one): the FIRST value of the field's
/// lines joined with ", " (`transport::mime::http_content_type`, A/B C5). Under `X-Content-Type-Options: nosniff`
/// that type must BE `text/css` (`IsSupportedStyleSheetMIMEType`): no `Content-Type`, an empty one and
/// `application/x-unknown-content-type` are refused there.
pub(crate) fn stylesheet_mime_ok(headers: &[(String, String)]) -> bool {
    let t = transport::mime::http_content_type(headers);
    if transport::mime::nosniff(headers) {
        return t.as_deref() == Some("text/css");
    }
    match t {
        None => true,
        Some(t) => t.is_empty() || t == "text/css" || t == "application/x-unknown-content-type",
    }
}

/// CSSOM's **origin-clean** flag for a sheet that has just arrived: same-origin, or a CORS fetch the
/// response permitted.
///
/// Measured on stock Chrome 150.0.7871.46 across two loopback origins. The ACAO header alone is not
/// enough — the REQUEST has to have been a CORS one, which for a `<link>` means the `crossorigin`
/// content attribute:
///
/// ```text
/// cross-origin, no crossorigin attr, no ACAO     cssRules -> SecurityError
/// cross-origin, no crossorigin attr, ACAO: *     cssRules -> SecurityError
/// cross-origin, crossorigin,         ACAO: *     cssRules -> 2 rules
/// cross-origin, crossorigin,         no ACAO     cssRules -> SecurityError
/// ```
///
/// An unparsable URL on either side is treated as NOT same-origin, which is the safe direction: a
/// sheet wrongly marked unreadable throws where Chrome answers, and a sheet wrongly marked readable
/// hands the page rules Chrome refuses it — the second is the one that silently inflates a
/// measurement, so the doubt goes the other way.
fn sheet_origin_clean(document_url: &str, sheet_url: &str, cors: bool, headers: &[(String, String)]) -> bool {
    let doc = url::Url::parse(document_url).ok();
    let sheet = url::Url::parse(sheet_url).ok();
    // A `data:` URL's response is "basic" (Fetch, main fetch: a current URL whose scheme is `data` sets
    // the response tainting to basic), so its sheet is origin-clean -- Chrome 150 hands a
    // `<link rel=stylesheet href="data:text/css,...">` its rules (measured 2026-10-08).
    if sheet.as_ref().is_some_and(|s| s.scheme() == "data") {
        return true;
    }
    if let (Some(d), Some(s)) = (&doc, &sheet) {
        if d.scheme() == s.scheme() && d.host_str() == s.host_str() && d.port_or_known_default() == s.port_or_known_default() {
            return true;
        }
    }
    if !cors {
        return false;
    }
    // The document's own origin, serialised the way an `Origin` header is.
    let allowed = match header_value(headers, "access-control-allow-origin") {
        Some(v) => v.trim().to_string(),
        None => return false,
    };
    if allowed == "*" {
        return true;
    }
    doc.map(|d| d.origin().ascii_serialization()).is_some_and(|o| o == allowed)
}

/// The bytes of a `<link rel=stylesheet>` have arrived and the response was 2xx: parse them into the
/// sheet the cascade holds for that element.
///
/// The rules land BEFORE the element's own `load` event fires, which is the order a `load` handler is
/// entitled to assume — the caller fires it a few lines further down.
///
/// Returns the event the link fires, `true` for `load`. A sheet its MIME type refuses ([`stylesheet_mime_ok`]) is
/// EMPTY and still fires `load`, unless `nosniff` refused it; and a QUIRKS document takes a CORS-same-origin sheet
/// whatever its type. Chrome 152.0.7977.82, 2026-10-23, one script-inserted link per row:
///
/// ```text
///                                           no-quirks document          quirks document
///   same-origin text/plain, text/html       load, 0 rules               load, its rule applied
///   data:text/plain, data:/,                load, 0 rules               load, its rule applied
///   cross-origin text/plain (no CORS)       load, SecurityError         load, SecurityError
///   text/plain + nosniff, either origin     error, SecurityError        error, SecurityError
///   text/css + nosniff                      load, its rules             load, its rules
/// ```
///
/// This engine fired `error` for every refused sheet, read a `nosniff`-refused one as an empty readable sheet, and
/// refused the quirks-mode ones too. (WPT `quirk-origin-check-recursive-import`'s one `error` is its failing
/// `@import`'s, not its `data:/,` sheet's.)
fn stylesheet_arrived(scope: &mut v8::PinScope, id: u32, body: &str, final_url: &str, headers: &[(String, String)]) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // The `crossorigin` content attribute is what made this a CORS request in `induced`; it is read
    // back here rather than carried through the completion, because the element is the thing that
    // owns it and a page may not change it in flight without re-triggering the load.
    let (document_url, cors) = {
        let st = op_state.borrow();
        let doc = st.try_borrow::<crate::core::dom::CookieUrl>().map(|c| c.0.clone()).unwrap_or_default();
        let cors = st
            .try_borrow::<HostDom>()
            .is_some_and(|h| h.0.get_attribute(id, "crossorigin").is_some());
        (doc, cors)
    };
    let clean = sheet_origin_clean(&document_url, final_url, cors, headers);
    if !stylesheet_mime_ok(headers) {
        let nosniff = transport::mime::nosniff(headers);
        let quirks = {
            let st = op_state.borrow();
            st.try_borrow::<HostDom>().is_some_and(|h| h.0.document_mode(h.0.shadow_including_root(id)) == korvus_dom::DocumentMode::Quirks)
        };
        if nosniff || !(quirks && clean) {
            let mut st = op_state.borrow_mut();
            let handle = crate::core::css::cssom::linked_sheet_loaded(&mut st, id, "", clean && !nosniff);
            crate::core::css::style_sheet::sheet_parsed_fresh(&mut st, handle);
            link_fetches(&mut st).bodies.insert(id, ("".into(), None, clean && !nosniff));
            return !nosniff;
        }
    }
    // What the link's next sheet holds if this one is detached ([`LinkSheetFetches`]`::bodies`).
    link_fetches(&mut op_state.borrow_mut()).bodies.insert(id, (body.into(), Some(final_url.to_string()), clean));
    // The sheet's own loads (`@import`, `url()`, `@font-face`) start here, before the
    // caller releases this link's load-delay token.
    crate::core::css::sheet_loads::linked_sheet_arrived(scope, id, body, final_url, clean);
    // New rules can move a deferred `loading=lazy` element toward the viewport.
    crate::core::html::lazy_load::invalidated(scope);
    true
}

// ---- the parser's script-blocking stylesheets -------------------------------------
//
// A parser-inserted script does not run while a stylesheet the parser met BEFORE it is still loading
// (HTML: "a style sheet that is blocking scripts"). Measured in Chrome 150 on `tools/realweb/seed/sb`:
// a head script after `<link rel=stylesheet href=s.css>` reads `document.styleSheets.length` 1 and the
// sheet's `font-family`; a body script after a second link reads that sheet's `width`. Korvus started
// every parsed stylesheet's fetch from `start_parsed_subresources`, and those fetches settle on the
// event loop -- after the whole script pass -- so every parser script, and `DOMContentLoaded`, read
// unstyled values (journals.plos.org: Foundation read `"Times New Roman"` as its data namespace).
//
// So the runtime fetches these sheets itself, beside the parser's scripts, and hands each one back
// before the first parser script that follows its link, through [`parser_sheet_arrived`] -- the style
// half of [`cb_load_ok`], run synchronously.

/// One parsed `<link rel=stylesheet>` whose load the runtime performs: the element, the URL and mode
/// [`induced`] gives it.
pub struct ParserSheet {
    pub id: u32,
    pub url: String,
    pub mode: &'static str,
    /// `"include"` for `crossorigin=use-credentials`, else `""`.
    pub credentials: &'static str,
}

/// The credentials mode `id`'s CORS settings attribute asks for: `"include"` for `use-credentials`, `""`
/// otherwise (the mode's own).
fn cors_credentials(op_state: &std::rc::Rc<RefCell<OpState>>, id: u32) -> &'static str {
    let st = op_state.borrow();
    let used = st.try_borrow::<HostDom>().and_then(|h| h.0.get_attribute(id, "crossorigin").map(|v| v.trim().eq_ignore_ascii_case("use-credentials")));
    if used == Some(true) { "include" } else { "" }
}

/// The parsed stylesheet links that precede a parser script in tree order, in tree order, each CLAIMED
/// so the loader's parsed-element pass does not issue it a second time.
pub(crate) fn claim_parser_sheets(scope: &mut v8::PinScope) -> Vec<ParserSheet> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let candidates: Vec<u32> = {
        let st = op_state.borrow();
        let Some(h) = st.try_borrow::<HostDom>() else { return Vec::new() };
        let dom = &h.0;
        let mut seen: Vec<u32> = Vec::new();
        let mut blocking = 0usize;
        for id in dom.descendant_elements(dom.root()) {
            let Some(tag) = dom.tag_name(id) else { continue };
            if tag.eq_ignore_ascii_case("script") && crate::core::html::html_script_element::element_script_type_runs(dom, id) {
                blocking = seen.len();
            } else if tag.eq_ignore_ascii_case("link") && page::is_stylesheet_link(dom, id) {
                seen.push(id);
            }
        }
        seen.truncate(blocking);
        seen
    };
    let mut out = Vec::new();
    for id in candidates {
        // Only a sheet whose `media` matches blocks the scripts after it. Chrome 150 (WPT
        // `link-stylesheet-with-non-match-media-does-not-block-render.tentative.html`): a
        // `<link rel=stylesheet media=print>` delayed a second is still pending when the next script
        // runs. The loader fetches it on its own, like any other sheet.
        let media = {
            let st = op_state.borrow();
            st.try_borrow::<HostDom>().and_then(|h| h.0.get_attribute(id, "media").map(str::to_string))
        };
        if let Some(m) = media.filter(|m| !m.trim().is_empty()) {
            if !crate::core::css::cssom::media_matches(scope, &m) {
                continue;
            }
        }
        let Some(ind) = induced(scope, id, "href") else { continue };
        // `data:`/`blob:` are answered locally by the loader's own op, not by the network client the
        // runtime's prefetch goes through; those stay the loader's.
        let local = ind.url.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("data:") || p.eq_ignore_ascii_case("blob:"));
        // a sheet with dangling markup stays the loader's, whose `issue` refuses it, and so does one the
        // page's CSP refuses: it is never requested and blocks no script
        if ind.destination != "style" || !ind.delays || local || ind.dangling || crate::core::html::csp::element_request_refused_quietly(scope, id, &ind.url, "style") {
            continue;
        }
        if !claim(&op_state, id, &claim_key(&ind.tag, ind.destination, ind.delays, &ind.url)) {
            continue;
        }
        crate::core::timing::resource_timing::note_initiator(scope, id, "link");
        // In flight until it arrives, as `issue` marks one: `link.sheet` is null meanwhile, so the
        // sheet is not in `document.styleSheets` before the parse reaches it (Chrome: `head:1`).
        link_sheet_fetch_started(&op_state, id, &ind.url);
        out.push(ParserSheet { id, url: ind.url, mode: ind.mode, credentials: cors_credentials(&op_state, id) });
    }
    out
}

/// A sheet [`claim_parser_sheets`] handed out has been fetched -- `None` when the request failed
/// outright: parse it into the cascade and fire the element's `load` (or `error`), exactly as the
/// loader's own completion does.
pub(crate) fn parser_sheet_arrived(scope: &mut v8::PinScope, id: u32, fetched: Option<(u16, Vec<(String, String)>, String, String)>) {
    // the URL `claim_parser_sheets` asked for: the element's `href`, resolved as `induced` resolves it
    let url = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let raw = op_state.borrow().try_borrow::<HostDom>().and_then(|h| h.0.get_attribute(id, "href").map(|v| v.trim().to_string())).unwrap_or_default();
        let url = resolve(&node_base_url(scope, id), &raw);
        link_sheet_fetch_ended(&op_state, id, &url);
        url
    };
    let is_sheet = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().is_some_and(|h| page::is_stylesheet_link(&h.0, id))
    };
    let ok = match &fetched {
        Some((status, headers, body, final_url)) if (200..300).contains(status) && is_sheet => {
            // in Blink's memory cache as a script-inserted link's is ([`cb_load_ok`]): an `href` that comes back to this
            // URL loads it again with no second resource-timing entry (Chrome 152, the verifier's rvpimp probe,
            // 2026-10-23; korvus listed one for every return of a parsed link)
            if fresh_for_reuse(headers) {
                let op_state = deno_core::JsRuntime::op_state_from(scope);
                link_fetches(&mut op_state.borrow_mut()).fresh.insert(url.clone());
            }
            stylesheet_arrived(scope, id, body, final_url, headers)
        }
        _ => false,
    };
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        link_sheet_outcome(&op_state, id, ok);
    }
    if !element_connected(scope, id) {
        return;
    }
    // Its `load` waits for its imports, as [`cb_load_ok`]'s does; the parse requests them once it is done.
    if ok && is_sheet {
        let token = new_load_delay_token(scope);
        delay_load(scope, token);
        if !sheet_load_settled(scope, id, token) {
            crate::core::css::sheet_loads::wait_for_imports(scope, id, token);
        }
        return;
    }
    fire(scope, id, if ok { "load" } else { "error" });
}

fn cb_load_err(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some(Completion { id, framing, script, style, delay, url: raw, silent }) = unpack(scope, __body_data) else { return };
    if script {
        // A FAILED FETCH IS A TASK TOO: HTML queues one to fire `error` at the element,
        // and running it here would fire that event from inside the microtask drain -- out of order
        // with the successful siblings this queue keeps it in line with.
        queue_settled_script(scope, id, ScriptDone { ok: false, body: String::new(), final_url: raw, delay });
        return;
    }
    if style && delay.is_some() && !link_fetch_current(scope, id, &raw) {
        link_fetch_cancelled(scope, id, &raw);
        if let Some(t) = delay {
            undelay_load(scope, t);
        }
        return;
    }
    let silent = style && silent && silent_sheet_end(scope, id, &raw);
    if style && !silent {
        link_sheet_ended(scope, id, &raw, delay.is_some());
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        link_sheet_outcome(&op_state, id, false);
    }
    // A frame whose navigation failed outright still ends the navigation, in an ERROR PAGE: Chromium commits
    // its network-error document at `kUnreachableWebDataURL` (`chrome-error://chromewebdata/`), cross-origin to
    // every page, and the frame element fires `load`. Chrome 150, measured (`fixtures/frame-status`): a frame
    // whose connection is refused, or a SAME-origin one whose connection closes with no response, reads
    // `contentDocument` null and throws SecurityError on `contentWindow.location.href`; this engine gave the
    // same-origin one an empty, readable document at the URL that failed. A `blob:` or `data:` URL keeps its
    // own path: a `blob:` the transport cannot fetch is still the Blob's bytes (`frame_navigated`).
    if framing {
        let null: v8::Local<v8::Value> = v8::null(scope).into();
        let network = url::Url::parse(&raw).is_ok_and(|u| matches!(u.scheme(), "http" | "https"));
        if network {
            // the error page answers no response's policies
            crate::core::html::browsing_context::frame_response_delivered(scope, id, &[], None);
        }
        let u = str_val(scope, if network { UNREACHABLE_WEB_DATA_URL } else { raw.as_str() });
        frame_navigated(scope, id, null, u);
        return;
    }
    note_object_outcome(scope, id, &raw, None);
    // An image whose fetch failed outright is settled and BROKEN before its `error` handler runs, as
    // one whose bytes did not decode is (`cb_load_ok`): `complete` reads `true` and the layout has its
    // fallback content (A/B C8). Harmless for a non-image id, which is never in the loading set.
    let is_img = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        mark_image_settled(&mut st, id);
        let is_img = st.try_borrow::<HostDom>().is_some_and(|h| h.0.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("img")));
        if is_img {
            crate::core::dom::geometry_queries::NaturalImages::set(&mut st, id, Some(page::ImageState::Broken));
        }
        is_img
    };
    if !silent && !(style && !element_connected(scope, id)) {
        fire(scope, id, "error");
    }
    // `img.decode()` called while the request was in flight rejects after the element's own event.
    if is_img {
        crate::core::html::html_image_element::settle_decodes(scope, id, false);
    }
    // A FAILED FETCH RELEASES EXACTLY LIKE A SUCCESSFUL ONE. HTML stops delaying the load event when
    // the fetch ENDS, however it ends; a release that only happened on success would turn one 404
    // into a whole document held to the cap, which reads in a test suite as a timeout -- "no verdict"
    // rather than "wrong".
    if let Some(t) = delay {
        undelay_load(scope, t);
    }
}

// ---- the two seams ----------------------------------------------------------------------------

/// The attributes "update the image data" reads: an `<img>`'s arm of [`maybe_load_for`].
const IMG_ATTRS: &[&str] = &["src", "srcset", "sizes", "loading", "crossorigin"];
/// The attributes of a `<picture>`'s `<source>` that re-select its image ([`maybe_load_for`]).
const PICTURE_SOURCE_ATTRS: &[&str] = &["src", "srcset", "sizes", "media", "type"];

/// Whether a write of attribute `name` can reach anything in [`resource_attr`] on an element that is NOT a
/// `<link>`: an EXACT SUPERSET of the names it and its callees act on, ASCII-case-insensitively, as they
/// compare --
///   * `on*`, the body's forwarded handlers ([`crate::core::html::html_body_element::body_handler_attr`]);
///   * `pattern` / `value` / `type` (`note_pattern`), `src` (a media element's load), `srcdoc` (a frame's);
///   * [`IMG_ATTRS`], [`PICTURE_SOURCE_ATTRS`] and a frame's `loading` ([`maybe_load_for`]'s routes);
///   * every attribute of every [`sources`] row (`src`, `srcset`, `poster`, `data`, `href`, `xlink:href`), the
///     only names [`destination_for`] answers for, so the only ones [`induced`] and [`blank_frame_load`] act on.
///
/// A `<link>` is the one element where every OTHER name still does something: the favicon list re-reads the link
/// on any write ([`resource_attr`]'s short path). `tests::attr_names_cover_every_route` pins the superset.
///
/// WHY: every attribute write in the engine runs this seam, and the full path lowercased the name three
/// times into the heap and took the arena twice to find that a `data-x` on a `<span>` loads nothing --
/// measured 2026-10-23 on the release build, `resource_attr` + `maybe_load_for` + `induced` + `sources`
/// were 13 % of the self time of an `el.setAttribute('data-x', v)` loop before their allocations.
fn attr_name_may_load(name: &str) -> bool {
    let b = name.as_bytes();
    if b.len() >= 2 && b[..2].eq_ignore_ascii_case(b"on") {
        return true;
    }
    // the longest name below is `crossorigin`; nothing longer can match
    let mut buf = [0u8; 11];
    let Some(l) = buf.get_mut(..b.len()) else { return false };
    l.copy_from_slice(b);
    l.make_ascii_lowercase();
    matches!(
        &*l,
        b"pattern" | b"value" | b"type" | b"src" | b"srcset" | b"sizes" | b"loading" | b"crossorigin" | b"media" | b"poster" | b"data" | b"href" | b"xlink:href" | b"rel" | b"srcdoc"
    )
}

/// Attribute seam: called from the native attribute choke point for every attribute write, whichever
/// JS-visible API made it (setAttribute, a reflected IDL setter, toggleAttribute).
pub(crate) fn resource_attr(scope: &mut v8::PinScope, target: v8::Local<v8::Value>, name: &str) {
    // Nearly every write loads nothing, and only needs to find that out ([`attr_name_may_load`]): the name
    // alone answers. On a `<link>` such a name loads nothing either -- [`destination_for`] answers for `href`
    // alone and `rel` passes the name test -- and all the full path did with it was [`maybe_load_for`]'s
    // favicon re-read, so that is all this does. Sending a `<link>` on down the full path paid for this test
    // and then for the full path's own lowercasing and arena reads: `link.setAttribute('title', v)` cost
    // 4,330 instructions a call against 4,016 without the test (main thread, 300k calls less none, min of 5,
    // 2026-10-23). Anything that might load takes the full path below, unchanged.
    if !attr_name_may_load(name) {
        // A `style`, `class` or `hidden` write can show a deferred `loading=lazy` element, or move it near: a carousel
        // turning its next slide from `display: none` to `block` (a production site's banner).
        if name.eq_ignore_ascii_case("style") || name.eq_ignore_ascii_case("class") || name.eq_ignore_ascii_case("hidden") {
            crate::core::html::lazy_load::invalidated(scope);
        }
        let Some(id) = unwrap_id(scope, target) else { return };
        let title = name.eq_ignore_ascii_case("title");
        let (link, minted) = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            let link = st.try_borrow::<HostDom>().is_some_and(|h| h.0.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("link")));
            (link, title && crate::core::css::style_sheet::minted_in(&st, id))
        };
        // A connected `<style>`'s or `<link>`'s sheet's title follows the attribute.
        if minted {
            crate::core::css::style_sheet::title_written(scope, id);
        }
        if link {
            crate::core::html::link_icons::link_changed(scope);
        }
        return;
    }
    let n = name.to_ascii_lowercase();
    // The native attribute choke point is SHARED, not subresource-only: it fires for every attribute
    // write. An `on<type>` written on a `<body>` is not a subresource, but it is an event-handler
    // content attribute that the body FORWARDS to the Window (core/html/html_body_element.rs), and
    // this is the only seam that sees it. Cheap to skip: two byte comparisons before anything else
    // runs. A direct Rust call -- it was a `bridge_get("__bodyHandlerAttr")` and
    // a JS call, because the body lived in a bridge slot.
    if n.as_bytes().starts_with(b"on") {
        crate::core::html::html_body_element::body_handler_attr(scope, target, &n);
    }
    let Some(id) = unwrap_id(scope, target) else { return };
    // A write that can change whether an `<input pattern>` matches: the arena needs the
    // script engine's answer before the cascade asks `:invalid`.
    if matches!(n.as_str(), "pattern" | "value" | "type") {
        crate::core::html::html_input_element::note_pattern(scope, id);
    }
    // A media element's `src`, set or changed, runs its load algorithm.
    if n == "src" {
        let media = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            st.try_borrow::<HostDom>().is_some_and(|h| crate::core::html::media_load::is_media(&h.0, id))
        };
        if media {
            crate::core::html::media_load::src_set(scope, id);
            return;
        }
    }
    // A connected frame's `srcdoc`, however the page wrote it, navigates the frame to it (HTML's "process the iframe
    // attributes"); only the IDL setter did, `setAttribute("srcdoc", ...)` did nothing. A frame in no document has
    // no browsing context to navigate until it is inserted (`resource_inserted`).
    if n == "srcdoc" {
        let html = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            st.try_borrow::<HostDom>().and_then(|h| {
                let frame = h.0.tag_name(id).is_some_and(|t| frameish(&t.to_ascii_lowercase()));
                frame.then(|| h.0.get_attribute(id, "srcdoc").map(str::to_string)).flatten()
            })
        };
        if let Some(html) = html {
            if home_of(scope, id) != Home::Nowhere {
                crate::core::html::browsing_context::frame_load_srcdoc(scope, id, &html);
            }
            return;
        }
    }
    // What a stylesheet link named before this write, which [`link_href_changed`] lets go of.
    let before = if n == "href" { link_wanted(scope, id) } else { None };
    maybe_load_for(scope, id, &n, false, Cause::Written);
    if n == "href" {
        link_href_changed(scope, id, before);
    }
}

/// A removed attribute (`remove_attribute_notify`): a connected frame that loses its `srcdoc` navigates to its `src`
/// when it has one, else to `about:blank` -- Chrome 150 (the frame reads `about:blank` and an empty body after it).
/// And a stylesheet `<link>` that loses its `disabled` loads now ([`disabled_stylesheet_link`]), one that loses its
/// `href` has no sheet ([`link_href_changed`]), and a `<style>`'s or `<link>`'s sheet's title follows a removed `title`
/// ([`crate::core::css::style_sheet::title_written`]: Chrome 152, `removeAttribute('title')` then the element removed,
/// the old sheet's `title` is null; the first cut answered the removed title).
pub(crate) fn resource_attr_removed(scope: &mut v8::PinScope, target: v8::Local<v8::Value>, name: &str) {
    if name.eq_ignore_ascii_case("title") {
        let Some(id) = unwrap_id(scope, target) else { return };
        crate::core::css::style_sheet::title_written(scope, id);
        return;
    }
    if name.eq_ignore_ascii_case("href") {
        let Some(id) = unwrap_id(scope, target) else { return };
        let before = link_wanted(scope, id);
        link_href_changed(scope, id, before);
        return;
    }
    if name.eq_ignore_ascii_case("disabled") {
        let Some(id) = unwrap_id(scope, target) else { return };
        let (sheet_link, ended_disabled) = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            let sheet_link = st.try_borrow::<HostDom>().is_some_and(|h| h.0.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("link")) && page::link_rel_has(&h.0, id, "stylesheet"));
            (sheet_link, st.try_borrow::<LinkSheetFetches>().is_some_and(|f| f.ended_disabled.contains(&id)))
        };
        // One whose fetch ended while it was disabled requests nothing and fires nothing ([`disabled_stylesheet_link`]).
        if sheet_link && !ended_disabled {
            // One that had loaded keeps its claim and loads nothing new; it fires its `load` again
            // ([`replay_link_load`]) with a new sheet.
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let loaded = link_sheet_settled(&op_state, id);
            maybe_load_for(scope, id, "href", false, Cause::Written);
            if loaded && link_sheet_settled(&op_state, id) {
                crate::core::css::style_sheet::detach_element_sheet(scope, id, crate::core::css::style_sheet::Fresh::Own);
                replay_link_load(scope, id);
            }
        }
        return;
    }
    if !name.eq_ignore_ascii_case("srcdoc") {
        return;
    }
    let Some(id) = unwrap_id(scope, target) else { return };
    let (frame, src) = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let Some(h) = st.try_borrow::<HostDom>() else { return };
        (h.0.tag_name(id).is_some_and(|t| frameish(&t.to_ascii_lowercase())), h.0.get_attribute(id, "src").is_some_and(|s| !s.trim().is_empty()))
    };
    if !frame || home_of(scope, id) == Home::Nowhere {
        return;
    }
    if src {
        maybe_load_for(scope, id, "src", false, Cause::Written);
    } else {
        crate::core::html::browsing_context::frame_load_blank(scope, id);
    }
}

/// Insertion seam: called from every arena insertion path. An inserted element may carry several
/// loadable attributes (a `<video src poster>`), and inserting a SUBTREE connects its descendants too,
/// which is how `<svg><image href>` loads -- the svg is what gets appended.
pub(crate) fn resource_inserted(scope: &mut v8::PinScope, node: v8::Local<v8::Value>) {
    let Some(id) = unwrap_id(scope, node) else { return };
    // An inserted `<input pattern>` (an `innerHTML` subtree included) needs its answer too.
    crate::core::html::html_input_element::note_patterns_under(scope, id);
    // A tree change can move a deferred `loading=lazy` element toward the viewport.
    crate::core::html::lazy_load::invalidated(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // Elements only, and the descendant walk in the SAME borrow: an insertion of a large tree is not a
    // load event for every node in it and this runs on the DOM's hot path.
    let ids: Vec<u32> = {
        let st = op_state.borrow();
        let Some(host) = st.try_borrow::<HostDom>() else { return };
        let dom = &host.0;
        if dom.node_type(id) != 1 {
            // Text inserted into a `<style>` makes its sheet again ([`queue_style_load`]).
            let style = dom.parent(id).filter(|&p| dom.tag_name(p).is_some_and(|t| t.eq_ignore_ascii_case("style")));
            drop(st);
            if let Some(style) = style {
                queue_style_load(scope, style);
            }
            return;
        }
        let mut out = vec![id];
        let mut i = 0;
        // SHADOW-INCLUDING: a host connected with an iframe, stylesheet or script inside
        // its shadow root connects those too -- a challenge provider an interactive-widget challenge puts its iframe in a closed root
        // of a host that is still detached, then appends the host. Chrome 150 loads all three.
        while i < out.len() {
            let n = out[i];
            if let Some(sr) = dom.shadow_root_of(n) {
                out.extend(dom.child_elements(sr));
            }
            out.extend(dom.child_elements(n));
            i += 1;
        }
        out
    };
    for n in ids {
        let tag = {
            let st = op_state.borrow();
            let Some(host) = st.try_borrow::<HostDom>() else { return };
            host.0.tag_name(n).map(|t| t.to_ascii_lowercase())
        };
        let Some(tag) = tag else { continue };
        // A `<style>` or `<link>` coming back into a tree had its sheet detached when it left: a new one is made
        // ([`crate::core::css::style_sheet::detach_element_sheet`]); a loaded stylesheet link fires its `load`
        // again ([`replay_link_load`]).
        if tag == "style" || tag == "link" {
            if crate::core::css::style_sheet::has_minted_sheet(&op_state, n) {
                crate::core::css::style_sheet::detach_element_sheet(scope, n, crate::core::css::style_sheet::Fresh::Own);
            }
            if tag == "link" && link_sheet_settled(&op_state, n) && home_of(scope, n) != Home::Nowhere {
                replay_link_load(scope, n);
            }
            if tag == "style" {
                queue_style_load(scope, n);
            }
        }
        // A media element's RESOURCE SELECTION runs at its insertion, and HTML's "honor user
        // preferences for automatic text track selection" is a step of it -- which is why Chrome
        // leaves a `<track default>` appended AFTERWARDS `disabled` while a parsed one is `showing`.
        // Latching the element here is what reproduces that.
        if matches!(tag.as_str(), "video" | "audio") {
            let el = crate::binding::protos::wrap_cached(scope, &op_state, n);
            crate::core::html::text_tracks::media_element_inserted(scope, el);
            // One made by markup (`innerHTML`) had its `src` and `<source>`s set by a parser that
            // went through neither seam; one a script made has already started.
            crate::core::html::media_load::parsed(scope, n);
        }
        // A `<source>` inserted into a media element can start, or resume, its resource selection.
        if tag == "source" {
            let media = {
                let st = op_state.borrow();
                st.try_borrow::<HostDom>().and_then(|h| h.0.parent(n).filter(|&p| crate::core::html::media_load::is_media(&h.0, p)))
            };
            if let Some(m) = media {
                crate::core::html::media_load::source_inserted(scope, m, n);
            }
        }
        // A frame inserted with a `srcdoc` navigates to `about:srcdoc` now that it has a browsing
        // context -- setting `srcdoc` on a frame in no document loads nothing (Chrome: no context, no
        // document). The parser's frames get theirs from the document install instead.
        if frameish(&tag) && home_of(scope, n) != Home::Nowhere {
            let srcdoc = op_state.borrow().try_borrow::<HostDom>().and_then(|h| h.0.get_attribute(n, "srcdoc").map(str::to_string));
            if let Some(html) = srcdoc {
                crate::core::html::browsing_context::frame_load_srcdoc(scope, n, &html);
            }
        }
        let Some(attrs) = sources(&tag).map(|rows| rows.iter().map(|(a, _)| *a).collect::<Vec<_>>()) else { continue };
        for a in attrs {
            maybe_load(scope, n, a, false);
        }
    }
}

// ---- THE THIRD SEAM: what the PARSER produced ------------------------------------
//
// The two seams above are an attribute WRITE and an arena INSERTION, which between them cover every
// element a SCRIPT creates or moves. The parser goes through neither: it builds its nodes straight
// into the flat arena. So until this pass every `<img src>`, `<link rel=stylesheet>`, `<object data>`
// and SVG `<image href>` written in the MARKUP -- which is how the overwhelming majority of the web
// writes them -- was never requested at all. MEASURED on a loopback server that logs paths: a page
// carrying a stylesheet and an image requested exactly one thing, itself; Chrome 150.0.7871.46 over
// the same server requested all three.
//
// Only frames were swept (`op_start_parsed_frames`, which this generalises), which is why
// `<iframe src>` worked and nothing else did.
//
// WHY A SWEEP AND NOT AN INSERTION NOTIFICATION FROM THE PARSER. The alternative considered was to
// give `korvus-dom`'s parser a callback per inserted element, so that the parser used the same
// insertion seam a script does. It costs more and buys nothing: the parse runs in `korvus-dom`,
// which knows nothing of V8 and holds no `PinScope` (`Dom::parse` is a pure function called before
// the document is installed), so the callback could not call `maybe_load` -- it would have to BUFFER
// the ids and hand them over later, which is this sweep with a new cross-crate channel bolted to the
// front, plus a second notification path alongside `resource_inserted` for the same fact. The sweep
// reads the finished tree once, in document order, and calls the ONE seam.
//
// WHAT IT DELIBERATELY DOES NOT SWEEP, and why each is a ruling rather than an oversight:
//
//   * `<script>`. `Engine::load_page` already fetches every external classic script itself and stages
//     the bodies for document-order execution (korvus-runtime/src/lib.rs); handing `<script>` to the
//     element loader as well would fetch every script on every page TWICE. Folding that staged fetch
//     into `maybe_load` is a real cleanup and a different unit -- it is document-order EXECUTION, not
//     a subresource load. Cost of the ruling: `<script src>` is still issued from a second place.
//
//   * A MEDIA BODY (`<video src>`, `<audio src>`, and a `<source>` inside either -- but NOT
//     `<video poster>`, which is an ordinary image). It is not skipped any more: it is the media
//     element's own RESOURCE SELECTION, which `core/html/media_load.rs` runs for every parsed media
//     element once this walk has collected them. Chrome requests these (measured:
//     `<video><source a><source b>` requests both, because it rejects the first and moves on), and
//     so does this engine now -- the WHOLE body, where Chrome stops after the metadata; the kernel's
//     loader reads the header and hands the page's realm no body at all.

/// The (tag, attribute) pairs the parser sweep leaves alone -- and only the SWEEP: a script that
/// creates the same element still loads it through the insertion seam, exactly as before.
///
///   * `<script src>` -- `Engine::load_page` already fetches it (see the block comment above).
///   * A MEDIA BODY: `<video src>`, `<audio src>`, and a `<source>` inside either -- the media
///     element's resource selection loads it (`media_load::parsed`). `<video poster>` is NOT a media
///     body -- it is an ordinary image, it is cheap, and it is swept.
fn parser_load_is_owned_elsewhere(tag: &str, attr: &str, in_media: bool) -> bool {
    match tag {
        "script" => true,
        "video" | "audio" => attr == "src",
        "source" => in_media,
        _ => false,
    }
}

/// Start every load the PARSER'S OWN elements induce, in document order, through [`maybe_load`] --
/// the same one seam a script-inserted element goes through.
///
/// Called once per document install, after the parse and before the page's own scripts, which is when
/// a real browser has already started them: that is what lets a script install `onload` afterwards and
/// still be called. `<iframe srcdoc>` is not here -- it needs no fetch, and `INSTALL_DOCUMENT` gives it
/// its document on every path.
///
/// The kernel's navigation pipeline is Rust and reaches this through [`crate::page`],
/// not through a JS string that looked an op up by name on the hidden bridge. The op that existed for
/// that older path, `op_start_parsed_subresources`, is deleted -- it had no caller.
pub(crate) fn start_parsed_subresources(scope: &mut v8::PinScope) {
    let root = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let Some(host) = st.try_borrow::<HostDom>() else { return };
        host.0.root()
    };
    sweep(scope, root);
}

/// The same sweep over a NESTED browsing context's document, once it has one -- an
/// `about:srcdoc` document, a `document.write()` into an initial `about:blank` frame, or a frame's
/// navigated response. Measured on Chrome 150.0.7871.46: `d.write("<img src=bw-img.png>...")` into
/// `iframe.contentDocument` requests `bw-img.png`; this engine built that tree through `DOMParser`,
/// which goes through neither seam, and requested nothing. The frame's scripts are not swept here for
/// the same reason the page's are not: the context's own script runner fetches and runs them.
pub(crate) fn start_frame_subresources(scope: &mut v8::PinScope, root: u32) {
    frame_scan_refusals(scope, root);
    sweep(scope, root);
    // A frame document's PARSER frames with a `srcdoc` navigate to `about:srcdoc` too, as the page's do at its install
    // (`page::frame_load_srcdoc`, the runtime's pipeline): only their `src` was swept, so a nested
    // `<iframe srcdoc="<script>...">` in a frame's markup never loaded. Chrome 150 runs it;
    // `srcdoc` wins over `src` (HTML "process the iframe attributes"), as `blank_frame_load` already says.
    let frames: Vec<(u32, String)> = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let Some(host) = st.try_borrow::<HostDom>() else { return };
        let dom = &host.0;
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            if dom.tag_name(n).is_some_and(|t| t.eq_ignore_ascii_case("iframe")) {
                if let Some(html) = dom.get_attribute(n, "srcdoc") {
                    out.push((n, html.to_string()));
                }
            }
            for k in dom.child_elements(n).into_iter().rev() {
                stack.push(k);
            }
        }
        out
    };
    for (n, html) in frames {
        crate::core::html::browsing_context::frame_load_srcdoc(scope, n, &html);
    }
}

/// A FRAME document's preload scanner, as far as its refusals go: each markup `<img>`, `<link>` and parser
/// `<script src>` its document's CSP refuses -- or whose URL has dangling markup -- has the scanner's entry, made before
/// the elements' own loads (the sweep below, the frame's script loop) make theirs. Chrome 150 x3: a refused
/// `<img>` in a frame document under its own CSP is listed twice on the FRAME's timeline. The page's own document has
/// [`preload_scan`] for this.
fn frame_scan_refusals(scope: &mut v8::PinScope, root: u32) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let candidates: Vec<(u32, String, Option<String>)> = {
        let st = op_state.borrow();
        let media = crate::core::dom::geometry_queries::media_context(&st);
        let Some(h) = st.try_borrow::<HostDom>() else { return };
        let dom = &h.0;
        dom.descendant_elements(root)
            .into_iter()
            .filter_map(|id| {
                let tag = dom.tag_name(id)?.to_ascii_lowercase();
                let raw = match tag.as_str() {
                    "img" if !is_lazy(dom, id) => image_source(dom, id, &media),
                    "script" if crate::core::html::html_script_element::element_script_type_runs(dom, id) => dom.get_attribute(id, "src").map(str::to_string),
                    "link" if !dom.get_attribute(id, "rel").is_some_and(crate::core::html::link_icons::is_icon_rel) => Some(String::new()),
                    _ => None,
                };
                Some((id, tag, raw))
            })
            .collect()
    };
    let local = |u: &str| u.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("data:") || p.eq_ignore_ascii_case("blob:"));
    for (id, tag, raw) in candidates {
        let home = home_of(scope, id);
        if !matches!(home, Home::Frame(_)) {
            continue;
        }
        let (url, refused) = if tag == "link" {
            let Some(ind) = induced(scope, id, "href") else { continue };
            if ind.lazy_frame || frameish(&ind.tag) || local(&ind.url) || ind.destination == "script" {
                continue;
            }
            let asked = if ind.destination.is_empty() { "prefetch" } else { ind.destination };
            let refused = ind.dangling || crate::core::html::csp::element_request_refused_quietly(scope, id, &ind.url, asked);
            (ind.url, refused)
        } else {
            let Some(raw) = raw.map(|r| r.trim().to_string()).filter(|r| !r.is_empty()) else { continue };
            let url = resolve(&base_for(scope, home), &raw);
            if local(&url) {
                continue;
            }
            let refused = dangling_http(&raw, &url)
                || if tag == "script" {
                    crate::core::html::csp::parser_script_url_refused_quietly(scope, id, &url)
                } else {
                    crate::core::html::csp::element_request_refused_quietly(scope, id, &url, "image")
                };
            (url, refused)
        };
        if refused {
            crate::core::timing::resource_timing::refused_load(scope, id, &tag, &url);
        }
    }
}

fn sweep(scope: &mut v8::PinScope, root: u32) {
    // The parser's `<input pattern>`s need the script engine's answer before the cascade asks
    // `:invalid`.
    crate::core::html::html_input_element::note_patterns_under(scope, root);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    // Every (element, attribute) the tables know, in DOCUMENT ORDER, collected in ONE borrow -- the
    // walk cannot be interleaved with `maybe_load`, which runs page JS and can mutate the arena.
    //
    // A `<template>`'s markup lives in its `content` fragment and never in its child list (see
    // `korvus-dom/src/parse.rs`), so a resource inside a template is not reached here -- which is
    // correct: a browser fetches nothing inside a template either.
    let (work, media): (Vec<(u32, &'static str)>, Vec<u32>) = {
        let st = op_state.borrow();
        let Some(host) = st.try_borrow::<HostDom>() else { return };
        let dom = &host.0;
        let mut out = Vec::new();
        let mut media = Vec::new();
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            if crate::core::html::media_load::is_media(dom, n) {
                media.push(n);
            }
            if let Some(tag) = dom.tag_name(n).map(|t| t.to_ascii_lowercase()) {
                // A `<source>` belongs to whatever parent it is in: inside a `<picture>` it is an
                // image candidate, inside a `<video>`/`<audio>` it is a media body.
                let in_media = dom
                    .parent(n)
                    .and_then(|p| dom.tag_name(p))
                    .is_some_and(|t| matches!(t.to_ascii_lowercase().as_str(), "video" | "audio"));
                if let Some(rows) = sources(&tag) {
                    out.extend(rows.iter().map(|(a, _)| (n, *a)).filter(|(_, a)| !parser_load_is_owned_elsewhere(&tag, a, in_media)));
                }
            }
            // Reversed, so popping the stack walks the children left to right.
            for k in dom.child_elements(n).into_iter().rev() {
                stack.push(k);
            }
        }
        (out, media)
    };
    for (id, attr) in work {
        let tag = {
            let st = op_state.borrow();
            st.try_borrow::<HostDom>().and_then(|h| h.0.tag_name(id).map(|t| t.to_ascii_lowercase()))
        };
        match tag.as_deref() {
            // ONE update per image, SYNCHRONOUSLY: the parser's images are requested as the document
            // is installed, before the page's scripts run (Chrome's preload scanner), not at the first
            // microtask checkpoint after them.
            Some("img") if attr == "src" => image_update(scope, id, false),
            Some("img") | Some("source") => {}
            _ => maybe_load_for(scope, id, attr, false, Cause::Parsed),
        }
    }
    // Each parsed `<video>`/`<audio>` starts its load, as the parser's creating it would.
    for id in media {
        crate::core::html::media_load::parsed(scope, id);
    }
}

// ---- the preload scanner ----------------------------------------------------------------------

/// One request the PRELOAD SCANNER issues for the parsed document, ahead of the element that will ask
/// for it: the element's own request -- the same URL, destination, mode, referrer policy and site for
/// cookies its [`issue`] would send -- so the element's load later takes this response
/// ([`EarlyFetches`]) instead of asking again.
pub struct ScannedLoad {
    pub id: u32,
    pub url: String,
    pub destination: &'static str,
    pub mode: &'static str,
    pub referrer_policy: &'static str,
    pub site_for_cookies: String,
    /// `"include"` for `crossorigin=use-credentials`, else `""`.
    pub credentials: &'static str,
}

/// The parsed document's images and `<link>` loads (preloads, stylesheets, fonts...) in tree order,
/// as Chrome's preload scanner finds them: requested at once, beside the parser's scripts. korvus asked
/// for them only after EVERY parser script and blocking stylesheet had arrived (the element sweep runs
/// after the fetch pass), so a page's resource timeline started with its scripts -- an interactive-widget challenge
/// widget walks `performance.getEntriesByType("resource")` for its own URL and met it FIRST, where
/// Chrome 150 lists the head's preloaded fonts and bundle before it (haveibeenpwned.com, replayed).
///
/// Not scanned, as Chrome's scanner does not request them: a lazy image, a `data:`/`blob:` URL, an
/// icon (the favicon handler's), a frame (a navigation), a script (the fetch pass's own), a CSP-refused
/// image, and the stylesheets in `skip` (the ones the fetch pass claimed). Nothing at all in an image document:
/// Blink's `ImageDocumentParser` is no HTML parser and has no scanner, and its `<img>` takes the navigation's
/// response ([`ImageDocumentContent`]).
pub fn preload_scan(scope: &mut v8::PinScope, skip: &[u32]) -> Vec<ScannedLoad> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if op_state.borrow().try_borrow::<ImageDocumentContent>().is_some_and(|c| c.open.contains_key(&None)) {
        return Vec::new();
    }
    let ids: Vec<(u32, String)> = {
        let st = op_state.borrow();
        let Some(h) = st.try_borrow::<HostDom>() else { return Vec::new() };
        let dom = &h.0;
        dom.descendant_elements(dom.root())
            .into_iter()
            .filter_map(|id| dom.tag_name(id).map(|t| (id, t.to_ascii_lowercase())))
            .filter(|(id, t)| (t == "img" || t == "link") && !skip.contains(id))
            .collect()
    };
    let site_for_cookies = crate::core::html::browsing_context::realm_site_for_cookies(scope);
    let local = |u: &str| u.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("data:") || p.eq_ignore_ascii_case("blob:"));
    let mut out = Vec::new();
    for (id, tag) in ids {
        if !matches!(home_of(scope, id), Home::Page) {
            continue;
        }
        let (url, destination, mode) = if tag == "img" {
            let (raw, lazy, crossorigin) = {
                let st = op_state.borrow();
                let media = crate::core::dom::geometry_queries::media_context(&st);
                let Some(host) = st.try_borrow::<HostDom>() else { continue };
                let dom = &host.0;
                (image_source(dom, id, &media), is_lazy(dom, id), dom.get_attribute(id, "crossorigin").is_some())
            };
            let Some(raw) = raw.map(|r| r.trim().to_string()).filter(|r| !r.is_empty()) else { continue };
            if lazy {
                continue;
            }
            let url = resolve(&base_for_detached(scope, Home::Page), &raw);
            // nor one with dangling markup: the element's own load refuses it, and Blink's scanner asks
            // `CanRequest` too, so it is never on the wire
            if local(&url) {
                continue;
            }
            // The scanner's refused request still has its entry, made at the refusal with no violation
            // (`resource_timing::refused_load`), and the element's own refusal makes a second: Chrome 150 x3
            // lists a refused markup `<img>` twice, an allowed one once.
            if !crate::core::html::csp::allows_image(scope, &url) || dangling_http(&raw, &url) {
                crate::core::timing::resource_timing::refused_load(scope, id, "img", &url);
                continue;
            }
            (url, "image", if crossorigin { "cors" } else { "no-cors" })
        } else {
            let icon = {
                let st = op_state.borrow();
                st.try_borrow::<HostDom>().and_then(|h| h.0.get_attribute(id, "rel").map(crate::core::html::link_icons::is_icon_rel)).unwrap_or(false)
            };
            if icon {
                continue;
            }
            let Some(ind) = induced(scope, id, "href") else {
                if let Some(name) = scanned_unloadable_sheet(scope, id) {
                    crate::core::timing::resource_timing::refused_load(scope, id, "link", &name);
                }
                continue;
            };
            if ind.lazy_frame || frameish(&ind.tag) || local(&ind.url) || ind.destination == "script" {
                continue;
            }
            // Refused -- by the page's CSP (the scanner's request asks it too, quietly) or as dangling markup -- it is
            // never requested, and has its entry (as an `<img>`'s above). Unasked, a refused stylesheet or
            // preload went on the wire here.
            let asked = if ind.destination.is_empty() { "prefetch" } else { ind.destination };
            if ind.dangling || crate::core::html::csp::element_request_refused_quietly(scope, id, &ind.url, asked) {
                crate::core::timing::resource_timing::refused_load(scope, id, "link", &ind.url);
                continue;
            }
            (ind.url, ind.destination, ind.mode)
        };
        let policy = crate::core::html::referrer_policy::element_request_policy(scope, id).or_else(|| crate::core::html::referrer_policy::running_policy(scope));
        out.push(ScannedLoad {
            id,
            url,
            destination,
            mode,
            referrer_policy: crate::core::html::referrer_policy::token(policy),
            site_for_cookies: site_for_cookies.clone(),
            credentials: cors_credentials(&op_state, id),
        });
    }
    out
}

/// A parsed stylesheet link -- or `rel=preload as=style` -- whose `href` Chrome's URL parser refuses ([`chrome_parses`]):
/// the element fetches nothing and fires nothing, but the preload scanner's request for it fails and leaves its
/// resource-timing entry, named as GURL spells the URL it could not take ([`chrome_invalid_spelling`]). Chrome
/// 152.0.7977.82, 2026-10-23, one parsed link per row, each entry `link`, duration 0, transfer size 0, status 0:
///
/// ```text
///   href="http://["  "http://[/x.css"  "https://["  "http://]"  "http://[::1"   http://[/  http://[/x.css  https://[/
///                                                                                 http://]/  http://[::1/
///   "http://a^b/"  "http://a<b/"  "http://a>b/"  "http://a|b/"  "http://%zz/"     http://a%5Eb/  http://a%3Cb/ ...
///   "http://[?q"  "http://[#f"  "http://user@a^b:80/p"                            http://[/?q  http://[/#f
///                                                                                 http://user@a%5Eb/p
///   "http://1.2.3.4.5/"  "http://127.0.0.1:99999/"                                as written
///   rel=stylesheet disabled, rel=preload as=style                                 an entry
///   rel="alternate stylesheet" title=t, rel=stylesheet media=print                none
/// ```
///
/// The build before put the stylesheet on the wire (an `error`, an entry named as written); the first fix left it no
/// entry at all.
fn scanned_unloadable_sheet(scope: &mut v8::PinScope, id: u32) -> Option<String> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (raw, media) = {
        let st = op_state.borrow();
        if !st.try_borrow::<LinkSheetFetches>().is_some_and(|f| f.unloadable.contains(&id)) {
            return None;
        }
        let dom = &st.try_borrow::<HostDom>()?.0;
        let sheet = page::link_rel_has(dom, id, "stylesheet") && !page::link_rel_has(dom, id, "alternate");
        if !sheet && !page::link_rel_has(dom, id, "preload") {
            return None;
        }
        let media = if sheet { dom.get_attribute(id, "media").map(str::to_string).filter(|m| !m.trim().is_empty()) } else { None };
        (dom.get_attribute(id, "href")?.trim().to_string(), media)
    };
    if media.is_some_and(|m| !crate::core::css::cssom::media_matches(scope, &m)) {
        return None;
    }
    Some(chrome_invalid_spelling(&resolve(&base_for_detached(scope, Home::Page), &raw)))
}

/// GURL's spelling of a URL it cannot take, which it keeps on the invalid URL ([`scanned_unloadable_sheet`]'s table):
/// the host's space, `<`, `>`, `^`, `|` and a `%` that starts no escape percent-encoded, a default port dropped, and a
/// `/` path where there is none.
fn chrome_invalid_spelling(url: &str) -> String {
    let Some(at) = url.find("://").map(|i| i + 3) else { return url.to_string() };
    let end = url[at..].find(['/', '?', '#', '\\']).map_or(url.len(), |e| at + e);
    let (user, hostport) = match url[at..end].rfind('@') {
        Some(a) => (&url[at..at + a + 1], &url[at + a + 1..end]),
        None => ("", &url[at..end]),
    };
    let (host, port) = if hostport.starts_with('[') {
        match hostport.find(']') {
            Some(b) if hostport[b + 1..].starts_with(':') => (&hostport[..=b], Some(&hostport[b + 2..])),
            _ => (hostport, None),
        }
    } else {
        match hostport.rfind(':') {
            Some(c) => (&hostport[..c], Some(&hostport[c + 1..])),
            None => (hostport, None),
        }
    };
    let mut out = String::with_capacity(url.len() + 8);
    out.push_str(&url[..at]);
    out.push_str(user);
    let hex = |b: Option<&u8>| b.is_some_and(u8::is_ascii_hexdigit);
    for (i, ch) in host.char_indices() {
        match ch {
            ' ' => out.push_str("%20"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            '^' => out.push_str("%5E"),
            '|' => out.push_str("%7C"),
            '%' if !(hex(host.as_bytes().get(i + 1)) && hex(host.as_bytes().get(i + 2))) => out.push_str("%25"),
            c => out.push(c),
        }
    }
    let scheme = &url[..at - 3];
    let default = (scheme.eq_ignore_ascii_case("http") && port == Some("80")) || (scheme.eq_ignore_ascii_case("https") && port == Some("443"));
    if let Some(p) = port.filter(|_| !default) {
        out.push(':');
        out.push_str(p);
    }
    let rest = &url[end..];
    if !rest.starts_with('/') {
        out.push('/');
    }
    out.push_str(rest);
    out
}

// ---- load priorities (the model and the measurements are `load_priority`'s) ----

/// Does `id`'s `blocking` attribute carry the `render` token?
fn blocking_render(dom: &korvus_dom::Dom, id: u32) -> bool {
    dom.get_attribute(id, "blocking").is_some_and(|b| b.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("render")))
}

/// A style sheet loaded at idle priority (`kIdleLoad`, Blink `LinkStyle::Process`): an alternate one, or
/// one whose `media` does not match.
fn sheet_idle(dom: &korvus_dom::Dom, id: u32, ctx: &page::style::media::MediaContext) -> bool {
    page::link_rel_has(dom, id, "alternate")
        || dom
            .get_attribute(id, "media")
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .is_some_and(|m| !page::style::media::MediaQueryList::parse(m, None).matches(ctx, &[]))
}

/// The `<video>`/`<audio>` a `<track>` belongs to: its CORS settings attribute.
fn media_parent_crossorigin(scope: &mut v8::PinScope, id: u32) -> Option<String> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let dom = &st.try_borrow::<HostDom>()?.0;
    let p = dom.parent(id).filter(|p| dom.tag_name(*p).is_some_and(|t| t.eq_ignore_ascii_case("video") || t.eq_ignore_ascii_case("audio")))?;
    dom.get_attribute(p, "crossorigin").map(str::to_string)
}

/// Is `id` before the document's body -- in its `<head>`? Blink's preload scanner treats a parser-blocking
/// script or a style sheet there as render-blocking, one in the body as "in-body parser-blocking".
fn in_head(dom: &korvus_dom::Dom, id: u32) -> bool {
    let mut n = dom.parent(id);
    while let Some(p) = n {
        if dom.tag_name(p).is_some_and(|t| t.eq_ignore_ascii_case("head")) {
            return true;
        }
        n = dom.parent(p);
    }
    false
}

/// The priority field of every load Blink's PRELOAD SCANNER makes for the document at `root`, walking it
/// in tree order as the scanner reads it (`load_priority::ScanState`): each parser `<script src>`, style
/// sheet and preload, and each image it requests (`<img>` not `loading=lazy`, `<input type=image>`, a
/// `<video poster>`; never a `data:`/`blob:` one, which it does not request). Recorded in
/// [`load_priority::ScannedPriorities`] for the loads the element loader makes -- a frame document's, and
/// the page's own that the runtime's fetch pass leaves to it -- and returned for the ones the runtime makes
/// itself. The page's document starts the table afresh; a frame's adds to it.
pub(crate) fn scan_load_priorities(scope: &mut v8::PinScope, root: u32) -> HashMap<u32, String> {
    use crate::core::html::html_script_element::{element_script_type_runs, is_module_element, nomodule_skips_element};
    use crate::core::html::load_priority::{absolute_dimension, field_for, Hint, Kind, Load, ScanState, ScannedPriorities};
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (out, page_root) = {
        let st = op_state.borrow();
        let ctx = crate::core::dom::geometry_queries::media_context(&st);
        let Some(h) = st.try_borrow::<HostDom>() else { return HashMap::new() };
        let dom = &h.0;
        let local = |u: &str| {
            let u = u.trim_start();
            u.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("data:") || p.eq_ignore_ascii_case("blob:"))
        };
        let attr_set = |id: u32, a: &str| dom.get_attribute(id, a).is_some_and(|v| !v.trim().is_empty());
        let mut scan = ScanState::default();
        let mut out = HashMap::new();
        for id in dom.descendant_elements(root) {
            let Some(tag) = dom.tag_name(id).map(|t| t.to_ascii_lowercase()) else { continue };
            let hint = Hint::parse(dom.get_attribute(id, "fetchpriority"));
            let render = blocking_render(dom, id);
            let load = match tag.as_str() {
                "script" => {
                    if !attr_set(id, "src") || !element_script_type_runs(dom, id) || nomodule_skips_element(dom, id) {
                        continue;
                    }
                    if is_module_element(dom, id) {
                        Load { classic: false, render_blocking: render, hint, ..Load::of(Kind::Script) }
                    } else {
                        let lazy = dom.get_attribute(id, "async").is_some() || dom.get_attribute(id, "defer").is_some();
                        Load { lazy, late: scan.image_seen, render_blocking: render || (!lazy && in_head(dom, id)), hint, ..Load::of(Kind::Script) }
                    }
                }
                "link" if attr_set(id, "href") => {
                    if page::is_stylesheet_link(dom, id) {
                        let idle = sheet_idle(dom, id, &ctx);
                        Load { idle, late: scan.image_seen, render_blocking: render || (!idle && in_head(dom, id)), hint, ..Load::of(Kind::Style) }
                    } else if page::link_rel_has(dom, id, "preload") {
                        let as_ = dom.get_attribute(id, "as").unwrap_or("").trim().to_ascii_lowercase();
                        let Some(kind) = preload_as_destination(&as_).and_then(Kind::of_destination) else { continue };
                        // a preload never counts as the document's image (`!is_link_preload`)
                        Load { link_preload: true, late: scan.image_seen, hint, ..Load::of(kind) }
                    } else {
                        continue;
                    }
                }
                "img" => {
                    let Some(src) = image_source(dom, id, &ctx).filter(|s| !s.trim().is_empty()) else { continue };
                    if is_lazy(dom, id) || local(&src) {
                        continue;
                    }
                    // `sizes` overrides a declared width (Blink `CreatePreloadRequest`)
                    let width = match dom.get_attribute(id, "sizes") {
                        Some(sizes) => Some(f64::from(crate::core::html::srcset::source_size(Some(sizes), &ctx))),
                        None => absolute_dimension(dom.get_attribute(id, "width")),
                    };
                    scan.image(width, absolute_dimension(dom.get_attribute(id, "height")), hint)
                }
                "input" if dom.get_attribute(id, "type").is_some_and(|t| t.trim().eq_ignore_ascii_case("image")) => {
                    let Some(src) = dom.get_attribute(id, "src").filter(|s| !s.trim().is_empty()) else { continue };
                    if local(src) {
                        continue;
                    }
                    scan.image(absolute_dimension(dom.get_attribute(id, "width")), absolute_dimension(dom.get_attribute(id, "height")), Hint::Auto)
                }
                "video" => {
                    let Some(src) = dom.get_attribute(id, "poster").filter(|s| !s.trim().is_empty()) else { continue };
                    if is_lazy(dom, id) || local(src) {
                        continue;
                    }
                    scan.image(None, None, Hint::Auto)
                }
                _ => continue,
            };
            out.insert(id, field_for(&load));
        }
        (out, dom.root())
    };
    let mut st = op_state.borrow_mut();
    if root == page_root || st.try_borrow::<ScannedPriorities>().is_none() {
        st.put(ScannedPriorities::default());
    }
    st.borrow_mut::<ScannedPriorities>().0.extend(out.iter().map(|(k, v)| (*k, v.clone())));
    out
}

/// The scanner's priority for `id`'s load, taken: only the load it was computed for gets it.
fn take_scanned_priority(scope: &mut v8::PinScope, id: u32) -> Option<String> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    st.try_borrow_mut::<crate::core::html::load_priority::ScannedPriorities>().and_then(|t| t.0.remove(&id))
}

/// An element's load the preload scanner did not make -- a script-inserted element, a rewritten source, a
/// lazy image coming into view -- as Blink's element loaders compute it: a script-inserted classic script
/// is `kLazyLoad` whether or not `async` was cleared (`!parser_inserted_`), a style sheet keeps its type's
/// priority unless idle, an image is Low. `None` leaves the destination's default.
fn element_load(dom: &korvus_dom::Dom, id: u32, tag: &str, destination: &str, ctx: &page::style::media::MediaContext) -> Option<crate::core::html::load_priority::Load> {
    use crate::core::html::load_priority::{Hint, Kind, Load};
    let hint = Hint::parse(dom.get_attribute(id, "fetchpriority"));
    let render = blocking_render(dom, id);
    Some(match (tag, destination) {
        ("script", _) if crate::core::html::html_script_element::is_module_element(dom, id) => {
            Load { classic: false, render_blocking: render, hint, ..Load::of(Kind::Script) }
        }
        ("script", _) => Load { lazy: true, render_blocking: render, hint, ..Load::of(Kind::Script) },
        ("link", "style") if page::is_stylesheet_link(dom, id) => Load { idle: sheet_idle(dom, id, ctx), render_blocking: render, hint, ..Load::of(Kind::Style) },
        ("link", d) if page::link_rel_has(dom, id, "preload") => Load { link_preload: true, hint, ..Load::of(Kind::of_destination(d)?) },
        ("link", "") if page::link_rel_has(dom, id, "prefetch") => Load::of(Kind::Prefetch),
        ("img", _) => Load { hint, ..Load::of(Kind::Image) },
        (_, "image") => Load::of(Kind::Image),
        _ => return None,
    })
}

/// [`LoadShape`] of `id`'s load: the scanner's priority when this is the load it computed one for, else
/// the element's own; and `include` credentials for `crossorigin=use-credentials` (a `<track>`'s is its
/// media element's attribute).
fn element_load_shape(scope: &mut v8::PinScope, id: u32, tag: &str, destination: &str) -> LoadShape {
    let scanned = take_scanned_priority(scope, id);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let ctx = crate::core::dom::geometry_queries::media_context(&st);
    let Some(h) = st.try_borrow::<HostDom>() else { return LoadShape::default() };
    let dom = &h.0;
    let cors = if tag == "track" {
        dom.parent(id).and_then(|p| dom.get_attribute(p, "crossorigin"))
    } else {
        dom.get_attribute(id, "crossorigin")
    };
    let credentials = if cors.is_some_and(|v| v.trim().eq_ignore_ascii_case("use-credentials")) { "include" } else { "" };
    let priority = scanned.or_else(|| element_load(dom, id, tag, destination, &ctx).map(|l| crate::core::html::load_priority::field_for(&l)));
    LoadShape { priority, credentials, parent: None }
}

/// The scanner's in-flight requests, by `(url, destination, mode)`: the element load that asks for the
/// same request takes it ([`op_subresource_fetch`]) -- Chrome's memory cache answering the parser's
/// element from the preload scanner's response, with no second request. Per document: cleared at each
/// navigation by the runtime.
#[derive(Default)]
pub struct EarlyFetches(pub HashMap<(String, String, String), deno_core::futures::future::Shared<spec::SubresourceFuture>>);

// ---- the op -----------------------------------------------------------------------------------

/// The engine's own loader, as an op so the fetch is a real async task with a real promise. Its body
/// is one line: hand the request to the kernel's [`spec::SubresourceFetch`] service. `korvus-webapi`
/// owns no HTTP client, no persona headers and no page location -- those are the kernel's, and this is
/// the seam between them.
///
/// It is not on the page's surface: `Deno.core.ops` is deleted before any page script runs, and the
/// only callers are the two seams above and a browsing context's own navigation.
/// The requesting document's side of an element load, as one op argument (async ops take at most nine):
/// its site for cookies, its URL when it is a frame's (T11), the referrer policy the
/// request goes out under, and the load's priority and credentials mode
/// (absent is the destination's default priority and the mode's credentials).
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubresourceClient {
    #[serde(default)]
    site_for_cookies: String,
    #[serde(default)]
    client_url: String,
    #[serde(default)]
    referrer_policy: String,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    credentials: String,
    /// A navigation's initiation, on the process clock (`spec::process_ms`): see `SubresourceRequest::navigation_start`.
    #[serde(default)]
    navigation_start: Option<f64>,
    /// A frame navigation's parent document's encoding and origin (`LoadShape::parent`).
    #[serde(default)]
    parent_encoding: Option<String>,
    #[serde(default)]
    parent_origin: Option<String>,
}

/// An element's load -- or a browsing context's navigation -- is REQUESTED AS IT IS ASKED FOR:
/// the op is `async(deferred)`, polled in the call that made it so the transport has the request before the task
/// that asked ends, and settled on a later turn whatever its first poll found. Chrome's network service has a
/// request the moment a page asks for it; korvus's op was `async(lazy)`, polled on the event loop's next turn -- a
/// frame's navigation left 14-29 ms after its element was inserted (Chrome 150's `fetchStart` 4-5 ms).
#[op2(async(deferred))]
#[serde]
pub(crate) async fn op_subresource_fetch(
    state: std::rc::Rc<RefCell<OpState>>,
    #[string] url: String,
    #[string] destination: String,
    #[string] mode: String,
    #[string] method: String,
    #[string] body: Option<String>,
    #[string] content_type: Option<String>,
    #[serde] client: SubresourceClient,
) -> Result<spec::SubresourceResponse, deno_error::JsErrorBox> {
    let SubresourceClient { site_for_cookies, client_url, referrer_policy, priority, credentials, navigation_start, parent_encoding, parent_origin } = client;
    let navigation_start = navigation_start.map(spec::process_instant);
    // The preload scanner already asked for exactly this ([`preload_scan`]): take its response. It asks for no
    // frame's document. (This first poll runs inside the call that asked for the load: the state is borrowed only
    // if no caller holds it.)
    let early = if mode == "navigate" {
        None
    } else {
        state.try_borrow_mut().ok().and_then(|mut st| st.try_borrow_mut::<EarlyFetches>().and_then(|e| e.0.remove(&(url.clone(), destination.clone(), mode.clone()))))
    };
    if let Some(f) = early {
        return f.await.map_err(deno_error::JsErrorBox::type_error);
    }
    // `data:` and `blob:` are LOCAL schemes (Fetch §4.2, scheme fetch): answered from the URL itself or from
    // the blob URL store, never handed to the network client -- which refused them (`builder error: invalid
    // format`, `uri too long`), so every data:/blob: image, script and style used to fail (69 of 271 sites,
    // measured 2026-09-24).
    if let Some(local) = local_scheme(&state, &url, &destination, &mode) {
        return local;
    }
    let svc = state
        .borrow()
        .try_borrow::<spec::kernel::Context>()
        .and_then(|c| c.get::<spec::SubresourceFetch>())
        .map(|h| h.0.clone());
    let Some(svc) = svc else {
        return Err(deno_error::JsErrorBox::type_error("no subresource loader on the kernel context"));
    };
    svc.fetch(spec::SubresourceRequest {
        url,
        destination,
        mode,
        method,
        body,
        content_type,
        site_for_cookies,
        client_url,
        referrer_policy,
        priority,
        credentials,
        navigation_start,
        parent_encoding,
        parent_origin,
    })
        .await
        .map_err(deno_error::JsErrorBox::type_error)
}

// The native ops this file owns -- declared here, beside their definitions, so that adding one touches
// no shared file. build.rs collects every such declaration into the extension's op table.
fn local_scheme(state: &std::rc::Rc<RefCell<OpState>>, url: &str, destination: &str, mode: &str) -> Option<Result<spec::SubresourceResponse, deno_error::JsErrorBox>> {
    let lower = url.get(..5).map(|p| p.to_ascii_lowercase()).unwrap_or_default();
    let found = if lower == "data:" {
        crate::modules::url::url::decode_data_url(url).map(|(m, b)| (m, std::sync::Arc::new(b)))
    } else if lower == "blob:" {
        crate::modules::url::url::blob_url_bytes(&state.borrow(), url)
    } else {
        return None;
    };
    let Some((mime, bytes)) = found else {
        return Some(Err(deno_error::JsErrorBox::type_error("Failed to fetch")));
    };
    let headers = if mime.is_empty() { Vec::new() } else { vec![("content-type".to_string(), mime)] };
    // A media body's header is read here and the body goes no further, exactly as the
    // kernel's loader does for one from the network.
    let media = matches!(destination, "video" | "audio");
    if media {
        if let Some(probes) = state.borrow().try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<crate::core::html::media_probe::MediaProbes>()) {
            let probe = crate::core::html::media_probe::probe(&bytes);
            probes.insert(url, probe.clone());
            if destination == "video" {
                probes.keep_body(url, probe.as_ref(), &bytes);
            }
        }
    }
    if destination == "font" {
        if let Some(fonts) = state.borrow().try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<crate::core::fonts::font_probe::FontProbes>()) {
            let ok = crate::core::fonts::font_probe::probe(&bytes);
            fonts.insert(url, ok);
            if ok {
                fonts.keep_body(url, &bytes);
            }
        }
    }
    if destination == "image" {
        if let Some(images) = state.borrow().try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::FetchedImages>()) {
            images.insert(url, mode, spec::FetchedImage { status: 200, final_url: url.to_string(), headers: headers.clone(), bytes: bytes.clone() });
        }
    }
    let body = if media { String::new() } else { String::from_utf8_lossy(&bytes).into_owned() };
    Some(Ok(spec::SubresourceResponse { status: 200, url: url.to_string(), headers, body, timing: None, encoding: None }))
}

crate::webapi_ops![op_subresource_fetch];

#[cfg(test)]
mod tests {
    use super::*;

    /// [`attr_name_may_load`] lets through every name the attribute seam acts on, in any ASCII case. The
    /// [`sources`] rows are read off this file's own text, so a row added there without a name here fails.
    #[test]
    fn attr_names_cover_every_route() {
        let src = include_str!("subresource.rs");
        let body = &src[src.find("fn sources(tag: &str)").unwrap()..];
        let body = &body[..body.find("_ => return None").unwrap()];
        let mut rows = 0;
        for line in body.lines().map(str::trim).filter(|l| l.starts_with('"') && l.contains("=> &[")) {
            let tag = line.split('"').nth(1).unwrap();
            let table = sources(tag).unwrap_or_else(|| panic!("`{tag}` has a row"));
            for (a, _) in table {
                assert!(attr_name_may_load(a), "{tag} {a}");
                assert!(attr_name_may_load(&a.to_ascii_uppercase()), "{tag} {a} (upper case)");
            }
            rows += 1;
        }
        assert!(rows >= 14, "read the source table ({rows} rows)");
        let routes = ["pattern", "value", "type", "src", "srcdoc", "loading", "rel", "href"];
        for a in IMG_ATTRS.iter().chain(PICTURE_SOURCE_ATTRS).chain(routes.iter()) {
            assert!(attr_name_may_load(a), "{a}");
            assert!(attr_name_may_load(&a.to_ascii_uppercase()), "{a} (upper case)");
        }
        for a in ["on", "onclick", "ONLOAD", "oNresize", "on-x", "onaveryveryverylongname"] {
            assert!(attr_name_may_load(a), "{a}");
        }
        // names no route reads, so a non-`<link>` element skips the seam for them
        for a in ["", "o", "n", "class", "id", "style", "title", "alt", "width", "height", "data-x", "hidden", "as", "crossoriginx", "srcsetx", "xlink:hreff", "\u{17f}rc", "valu", "datas"] {
            assert!(!attr_name_may_load(a), "{a:?}");
        }
    }

    /// The names Chrome 152 gives the preload scanner's entries for parsed stylesheet `href`s its URL parser refuses
    /// ([`scanned_unloadable_sheet`]), each as measured, 2026-10-23.
    #[test]
    fn an_unparseable_href_is_spelled_as_chrome_spells_it() {
        for (href, name) in [
            ("http://[", "http://[/"),
            ("http://[/x.css", "http://[/x.css"),
            ("https://[", "https://[/"),
            ("http://]", "http://]/"),
            ("http://[::1", "http://[::1/"),
            ("http://a^b/", "http://a%5Eb/"),
            ("http://a^b", "http://a%5Eb/"),
            ("http://a<b/", "http://a%3Cb/"),
            ("http://a>b/", "http://a%3Eb/"),
            ("http://a|b/", "http://a%7Cb/"),
            ("http://%zz/", "http://%25zz/"),
            ("http://a%zzb/q?x#f", "http://a%25zzb/q?x#f"),
            ("http://[?q", "http://[/?q"),
            ("http://[#f", "http://[/#f"),
            ("http://user@a^b:80/p", "http://user@a%5Eb/p"),
            ("http://1.2.3.4.5/", "http://1.2.3.4.5/"),
            ("http://127.0.0.1:99999/", "http://127.0.0.1:99999/"),
        ] {
            assert!(!chrome_parses(href), "{href} is refused");
            assert_eq!(chrome_invalid_spelling(href), name, "{href}");
        }
    }
}
