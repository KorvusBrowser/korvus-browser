//! `core/css/sheet_loads.rs` -- the loads a STYLESHEET induces (L17's E1).
//!
//! An element loads what its attributes name (`core/html/subresource.rs`). A stylesheet loads three
//! more things, none of which any attribute names, and until this file this engine issued none of
//! them. Measured on `/usr/bin/google-chrome` 150.0.7871.46 over a loopback server's access log
//! (fixtures `css.html`, `t1.html`, `t2.html`, `t3.html` in the unit's scratch):
//!
//! ```text
//! @import url(imp.css)                              fetched, destination style, before load
//! @import url(x) print                              fetched (the media list only gates the rules)
//! @import url(x) supports(display: nope)            NOT fetched, and not in cssRules
//! @import after a style rule                        NOT fetched (the rule is invalid)
//! .used { background: url(a.png) } on an element   fetched, destination image, priority Low
//! the same rule matching nothing / under display:none / overridden by a later winner   NOT fetched
//! visibility:hidden, opacity:0, 0x0, 9000px below the fold                            fetched
//! list-style-image, border-image-source, mask-image, cursor, content: url() on an element   fetched
//! mask-image / clip-path / filter / fill / marker url(#id) on an element                  NOT fetched -- see [`UrlLoad`]
//! image-set(url(a) 1x, url(b) 2x)                   a only
//! @font-face { font-family: Used } used by text     fetched, destination font
//! a face no element uses / used only by an empty or whitespace-only element / by display:none
//! text / behind an installed family earlier in the list / outside its unicode-range      NOT fetched
//! weight 400 and 700 faces, normal text             the 400 face only
//! four faces of one family, disjoint unicode-ranges  every face the TEXT'S CHARACTERS need, and no
//!                                                    other (`e21.html`)
//! `font-family: Chain1, Chain2`, Chain1 latin-only   f_c1 for the latin characters AND f_c2 for the
//!                                                    cyrillic one -- the list is walked per character
//! two faces of one family whose ranges OVERLAP       the one declared LAST (`e21b.html`)
//! an element holding only `&nbsp;`                   fetched; only ASCII spaces, NOT fetched
//! `.late` text replaced by script after load         the face the new characters need
//! src: local(x), url(a) format("embedded-opentype"), url(b) format("woff2"), url(c)   b only
//! load event                                        waits for all of them (a 700 ms font: load at 787 ms)::
//! before { content: ""; background: url(a.png) }  fetched                 (pseudo.html)
//! ::after { content: url(b.png) }                   fetched
//! ::before { content: "X"; font-family: Face }      the face fetched
//! ::before with no `content`, or under display:none  NOT fetched
//! script-set el.style.backgroundImage after load    fetched at the next rendering opportunity
//! ```
//!
//! # When a scan runs
//!
//! Chrome discovers these at style recalc, which runs at every rendering opportunity. This engine has
//! no frame clock, so a SCAN runs at the moments something that feeds it has changed and the load
//! event could still be delayed -- the document's parse ([`document_parsed`]), a stylesheet's arrival
//! ([`linked_sheet_arrived`]), an imported sheet's arrival -- and on a low-rate, UNREF'D frame
//! ([`arm_frame`]) that only re-scans when the cascade's fingerprint moved. Unref'd means the frame
//! never holds the event loop open by itself: a page with nothing scheduled still reads as drained.
//!
//! # What it does not do
//!
//! Decode anything: an image or a font is REQUESTED, which is what the network sees; its bytes are
//! not parsed (R7). Which named local families a persona has installed is not known (the
//! persona carries metrics for three), so a family that is neither a generic keyword nor one of those
//! is treated as NOT installed, which is Chrome's answer on this host for `"Segoe UI"`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use deno_core::{op2, v8, OpState};

use crate::binding::protos::{build_fn_data, get_prop, str_val};
use crate::binding::HostDom;
use crate::core::css::cssom;
use crate::core::html::browsing_context::{delay_load, new_load_delay_token, undelay_load};
use crate::core::html::isolation_policy::headers_from_js;
use page::style::property::PropertyId;
use page::style::rules::{RuleKind, Stylesheet};
use page::PageHandle;

/// How often the rendering-opportunity frame looks for a changed cascade, in milliseconds.
const FRAME_MS: u32 = 100;

/// This realm's style-induced load state, per document.
#[derive(Default)]
pub(crate) struct SheetLoads {
    /// Absolute URLs of images and fonts already requested by this document.
    requested: HashSet<String>,
    /// Imports already requested, by `(sheet handle, rule path, URL)`. The URL is part of the key because a sheet's
    /// rules are replaced from new text, and a rule at the same place can import another URL: keyed on the place
    /// alone, `<style>` text replaced by `@import url(a.css)` where it had imported `b.css` never requested `a.css`,
    /// and its `load` -- and the window's, which it delays -- waited for it until the starvation watchdog (Chrome 152,
    /// 2026-10-23: `a.css` requested, three `load`s, then the window's).
    imports: HashSet<(u32, Vec<u32>, String)>,
    /// An import URL's arrived body -- a second `@import` of the same URL takes it without a second
    /// request (Chrome requests `dup.css` once for two rules) -- or `None` while in flight, with the
    /// rules waiting on it.
    import_bodies: HashMap<String, Option<(String, String)>>,
    import_waiters: HashMap<String, Vec<(u32, Vec<u32>)>>,
    /// The scan key at the last scan ([`scan_key`], [`cb_frame`]).
    last: Option<u64>,
    /// Whether the sheets name `attr(style)` ([`sheets_name_attr_style`]), with the page geometry fingerprint it was
    /// asked at: that moves with every sheet change, so under an equal one the answer stands.
    attr_style: Option<(u64, bool)>,
    /// Each sheet's own answer, with what identifies the value pool it was read from ([`pool_key`]).
    attr_style_sheets: HashMap<u32, ([usize; 4], bool)>,
    frame_armed: bool,
    /// Bumped per document, so a frame or a fetch that outlives its document does nothing.
    epoch: u32,
    /// Import URLs whose fetch failed, or that were refused ([`import_state`]): a CSP or unfetchable one stays unloaded
    /// for good, one that was answered has an empty sheet ([`settle_import`]).
    import_failed: HashSet<String>,
    /// Imported sheets script may not read although their URL is the document's origin -- a network error, a `nosniff`
    /// refusal -- by the URL their sheet answers as its `href` ([`import_readable`]).
    import_unclean: HashSet<String>,
    /// `<style>` elements whose `load`/`error` waits for their sheet's imports, each with its load-delay token
    /// ([`wait_for_imports`]).
    style_waiters: Vec<(u32, u32)>,
}

fn with_loads<R>(st: &mut OpState, f: impl FnOnce(&mut SheetLoads) -> R) -> R {
    if st.try_borrow::<SheetLoads>().is_none() {
        st.put(SheetLoads::default());
    }
    f(st.borrow_mut::<SheetLoads>())
}

/// A new document: forget what the last one requested.
pub(crate) fn reset(op_state: &std::rc::Rc<RefCell<OpState>>) {
    let mut st = op_state.borrow_mut();
    with_loads(&mut st, |l| {
        let epoch = l.epoch.wrapping_add(1);
        *l = SheetLoads { epoch, ..SheetLoads::default() };
    });
}

/// The parser has finished building the document: start what its stylesheets induce.
pub(crate) fn document_parsed(scope: &mut v8::PinScope) {
    scan(scope);
    arm_frame(scope);
}

/// A `<link rel=stylesheet>`'s 2xx CSS body has arrived for `node`, fetched from `final_url`. Parse
/// it into the element's sheet (resolving its relative URLs against `final_url`), then start the
/// loads it induces -- BEFORE the caller releases the link's own load-delay token, so a document
/// waiting on nothing else still waits for these.
pub(crate) fn linked_sheet_arrived(scope: &mut v8::PinScope, node: u32, body: &str, final_url: &str, origin_clean: bool) {
    {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let id = cssom::linked_sheet_loaded(&mut st, node, body, origin_clean);
        if let Some(p) = st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<PageHandle>()).map(|h| h.0.clone()) {
            p.with_sheet_mut(id, |s| s.base_url = Some(final_url.to_string()));
        }
        crate::core::css::style_sheet::sheet_parsed_fresh(&mut st, id);
    }
    // A PAGE sheet that arrives while the parser's scripts are still running (a script-blocking one)
    // waits for the first style recalc like the rest of the document's styles
    // ([`document_parsed`], called once those scripts have run).
    let parsing = crate::core::html::browsing_context::doc_readiness(scope) == crate::core::html::browsing_context::Readiness::Loading
        && crate::core::html::subresource::home_of(scope, node) == crate::core::html::subresource::Home::Page;
    if parsing {
        return;
    }
    scan(scope);
    arm_frame(scope);
}

// ── the scan ─────────────────────────────────────────────────────────────────────────────────────

fn page_of(st: &OpState) -> Option<std::sync::Arc<page::Page>> {
    st.try_borrow::<spec::kernel::Context>()?.get::<PageHandle>().map(|h| h.0.clone())
}

/// One load to start: URL, destination, mode.
struct Load {
    url: String,
    destination: &'static str,
    mode: &'static str,
    /// Requested from an origin-dirty style sheet ([`dirty_bases`]): no resource-timing entry.
    dirty: bool,
}

/// Look at the document once and start every load it now induces that has not been started.
fn scan(scope: &mut v8::PinScope) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (imports, loads) = {
        let mut st = op_state.borrow_mut();
        let Some(p) = page_of(&st) else { return };
        let media = cssom::media_context(&st);
        let doc_base = crate::core::dom::base_url_of(&st);
        let doc_url = crate::core::dom::document_url_of(&st);
        let Some(host) = st.try_borrow::<HostDom>() else { return };
        let dom = &host.0;
        p.sync_document_sheets(dom);
        let fingerprint = p.page_geometry_fingerprint(dom, &media);
        let inert = dom.inert_style_writes();
        let imports = pending_imports(&p, &doc_base, &doc_url);
        let (loads, used_faces) = induced_loads(&p, dom, &media, &doc_base, &doc_url);
        let (pending, (mut loads, used_faces)) = with_loads(&mut st, |l| {
            l.last = Some(scan_key(l, &p, fingerprint, inert));
            // An import whose URL has ARRIVED is attached from the body already in hand, whether or
            // not this rule asked before: a `<style>`'s sheet is re-parsed from its text when script
            // first reads `.sheet`, which drops the children an earlier arrival attached.
            let mut attach_now = Vec::new();
            let mut request = Vec::new();
            for (h, path, url, dirty) in imports {
                match l.import_bodies.get(&url) {
                    Some(Some((final_url, body))) => attach_now.push((h, path, final_url.clone(), body.clone())),
                    _ if l.imports.insert((h, path.clone(), url.clone())) => request.push((h, path, url, dirty)),
                    _ => {}
                }
            }
            let loads: Vec<Load> = loads.into_iter().filter(|load| l.requested.insert(load.url.clone())).collect();
            ((attach_now, request), (loads, used_faces))
        });
        // An image the document now requests is origin-dirty when only origin-dirty rules name it: asked once
        // a scan starts one, not on every scan.
        if loads.iter().any(|l| l.destination == "image") {
            let dirty = origin_dirty_urls(&p, &doc_base, &doc_url);
            for load in loads.iter_mut().filter(|l| l.destination == "image") {
                load.dirty = dirty.contains(&load.url);
            }
        }
        (pending, (loads, used_faces))
    };
    let ((attach_now, request), (loads, used_faces)) = (imports, loads);
    for (handle, path, url, dirty) in request {
        start_import(scope, handle, path, url, dirty);
    }
    for load in loads {
        start_load(scope, &load);
    }
    // The faces layout needs are the document's CSS-connected `FontFace`s too: each starts loading there (F9).
    crate::core::fonts::font_face::css_faces_used(scope, &used_faces);
    let mut attached = false;
    for (handle, path, final_url, body) in &attach_now {
        attached |= attach(scope, *handle, path, final_url, body);
    }
    // Each pass attaches at least one import or stops, and a sheet has finitely many.
    if attached {
        scan(scope);
    }
    // An import attached from a body already in hand settles a `<style>` waiting on it too.
    let waiting = {
        let mut st = op_state.borrow_mut();
        with_loads(&mut st, |l| !l.style_waiters.is_empty())
    };
    if waiting {
        wake_style_waiters(scope);
    }
}

/// What a scan reads, as one number: the page's geometry fingerprint -- the PAGE document's rendered elements, their
/// text and computed styles, the sheets, the viewport, the container sizes and the animated values -- and, when a
/// sheet names `attr(style)`, the page's inert `style` rewrites as well.
///
/// The geometry fingerprint leaves out a `style` rewrite that changed only the transform family, `opacity` or what
/// only paint reads ([`korvus_dom::Dom::inert_style_writes`]), unless a SELECTOR reads the attribute. Generated content
/// can show the attribute's text itself, `::before { content: attr(style) }`, and the characters of that text pick
/// the `unicode-range` faces it needs. Chrome 152.0.7977.82, 2026-10-23, real-time headless over CDP, `@font-face
/// { unicode-range: U+36 }` behind `#e::before { content: attr(style) }`, `<div id=e style="opacity: 0.5;">`, and
/// `e.style.opacity = '0.6'` 150 ms after load: the face is requested, with frames running or with a layout forced
/// (`getBoundingClientRect`). This engine, keyed on the geometry alone, never scanned again and never asked for it.
/// The inert writes count only where a sheet's values name `attr(style)`, so a page without one keeps a scan that a
/// transform or opacity animation does not move.
fn scan_key(l: &mut SheetLoads, p: &page::Page, fingerprint: u64, inert: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let names = match l.attr_style {
        Some((at, names)) if at == fingerprint => names,
        _ => {
            let names = sheets_name_attr_style(l, p);
            l.attr_style = Some((fingerprint, names));
            names
        }
    };
    if !names {
        return fingerprint;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (fingerprint, inert).hash(&mut h);
    h.finish()
}

/// Does any sheet name `attr(style)` ([`scan_key`])? Asked of each sheet's value pool ([`Stylesheet::text`]), which
/// holds every value its rules have -- an inserted rule's and a CSSOM-set value's included -- and some they had, so
/// a stale yes is possible and a wrong no is not.
///
/// Each sheet's answer is kept under its pool's identity ([`pool_key`]) and read again only when that moves. Asked
/// under the geometry fingerprint alone, which moves with every DOM, text and geometry change, each frame that moved
/// geometry read every byte of every sheet, the UA sheet's included: about 1.1M instructions a frame over 1.07 MB of
/// CSS, 4.7 % of a page animating one width every 16 ms (`anim_big.html`, 2026-10-23).
fn sheets_name_attr_style(l: &mut SheetLoads, p: &page::Page) -> bool {
    let cache = &mut l.attr_style_sheets;
    let mut any = false;
    for h in 0..p.sheet_count() as u32 {
        any |= p
            .with_sheet(h, |s| {
                let key = pool_key(s);
                match cache.get(&h) {
                    Some(&(k, names)) if k == key => names,
                    _ => {
                        let names = names_attr_style(s.text());
                        cache.insert(h, (key, names));
                        names
                    }
                }
            })
            .unwrap_or(false);
    }
    any
}

/// What identifies a sheet's value pool for [`sheets_name_attr_style`]: the pool's address and length, and where its
/// rules and declarations live and how many declarations point into it. A CSSOM edit that adds a value appends it to
/// the pool, which moves the length (or the address); a sheet parsed afresh has its own allocations, made while the
/// old ones were still alive.
fn pool_key(s: &Stylesheet) -> [usize; 4] {
    [s.text().as_ptr() as usize, s.text().len(), s.rules().as_ptr() as usize ^ s.declarations().as_ptr() as usize, s.declarations().len() + s.raw_declarations().len()]
}

/// Does `text` call `attr()` on the `style` attribute? The name is matched as CSS reads it: the function name and the
/// attribute name ASCII case-insensitively, whitespace and comments between `(` and the name skipped. A name that is
/// escaped, namespaced (`attr(*|style)`) or not ASCII counts as `style`, so the answer errs to yes; an escaped
/// FUNCTION name (`\61ttr(`) is not read.
fn names_attr_style(text: &str) -> bool {
    let b = text.as_bytes();
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c >= 0x80;
    let mut from = 0;
    while let Some(k) = b[from..].iter().position(|&c| c == b'(') {
        let open = from + k;
        from = open + 1;
        if open < 4 || !b[open - 4..open].eq_ignore_ascii_case(b"attr") || (open > 4 && (ident(b[open - 5]) || b[open - 5] == b'\\')) {
            continue;
        }
        let mut i = from;
        loop {
            while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r' | b'\x0c') {
                i += 1;
            }
            if !b[i..].starts_with(b"/*") {
                break;
            }
            match b[i + 2..].windows(2).position(|w| w == b"*/") {
                Some(e) => i += 2 + e + 2,
                None => return true,
            }
        }
        let start = i;
        while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'-' || b[i] == b'_') {
            i += 1;
        }
        if b.get(i).is_some_and(|&c| c == b'\\' || c == b'|' || c >= 0x80) || b.get(start) == Some(&b'*') || b[start..i].eq_ignore_ascii_case(b"style") {
            return true;
        }
    }
    false
}

/// Where sheet `handle`'s `@import`s stand, nested ones included: [`ImportState::Pending`] while one an
/// `@import` would load has not arrived nor failed -- requested or about to be, as [`pending_imports`] lists them --
/// else [`ImportState::Failed`] when one failed, or names a URL that does not resolve (`@import url('x/')` in a
/// `data:` sheet, WPT `quirk-origin-check-recursive-import`), else [`ImportState::Done`].
///
/// THE ONE SHEET ONLY. It ran [`pending_imports`] over every rule of every sheet and kept this one's, so a page that
/// inserts N `<style>` elements -- CSS-in-JS outside its speedy mode, a component's `<style>` per shadow root, Vue's
/// style injection -- paid N times all its rules: 1,000 two-rule styles appended in one task cost 858M instructions
/// where the build before cost 120M, and 4,000 cost 10.7G against 0.58G (2026-10-23). A sheet with no `@import`
/// rule -- nearly every injected `<style>` -- answers at once.
pub(crate) fn import_state(scope: &mut v8::PinScope, handle: u32) -> ImportState {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let Some(p) = page_of(&st) else { return ImportState::Done };
    if !p.with_sheet(handle, |s| s.rules().iter().any(|r| matches!(r.kind, RuleKind::Import(_)))).unwrap_or(false) {
        return ImportState::Done;
    }
    let doc_base = crate::core::dom::base_url_of(&st);
    let doc_url = crate::core::dom::document_url_of(&st);
    let mut urls = Vec::new();
    let mut unresolved = false;
    p.with_sheet(handle, |sheet| {
        sheet_imports(sheet, &doc_base, &doc_url, &mut |_, url, _, cyclic| {
            if network_url(&url) {
                if !cyclic {
                    urls.push(url);
                }
            } else if url::Url::parse(&url).is_err() {
                unresolved = true;
            }
        })
    });
    // An import that was answered has its sheet, an empty one when the answer failed ([`settle_import`]).
    let mut answered = Vec::new();
    p.with_sheet(handle, |sheet| loaded_imports(sheet, &doc_base, &mut answered));
    with_loads(&mut st, |l| {
        if urls.iter().any(|u| !l.import_failed.contains(u)) {
            ImportState::Pending
        } else if unresolved || !urls.is_empty() || answered.iter().any(|u| l.import_failed.contains(u)) {
            ImportState::Failed
        } else {
            ImportState::Done
        }
    })
}

/// The URL every LOADED `@import` of `sheet` was requested at, nested ones included -- [`sheet_imports`]'s other half.
fn loaded_imports(sheet: &Stylesheet, doc_base: &str, out: &mut Vec<String>) {
    fn walk(sheet: &Stylesheet, rules: &[page::style::rules::Rule], base: &str, out: &mut Vec<String>) {
        for rule in rules {
            if let RuleKind::Import(import) = &rule.kind {
                if import.loaded {
                    out.push(page::style::urls::resolve(base, &import.href));
                    walk(sheet, &rule.children, sheet.base_of(import.base).unwrap_or(base), out);
                }
            }
        }
    }
    walk(sheet, sheet.rules(), sheet.base_url.as_deref().unwrap_or(doc_base), out);
}

/// May script read an imported sheet fetched from `href` (`CSSImportRule.styleSheet.cssRules`)? Only when its response
/// was CORS-same-origin: an import is a `no-cors` request, so one of another origin is not, nor is a network error or a
/// `nosniff` refusal ([`settle_import`]). Chrome 152.0.7977.82, 2026-10-23: a cross-origin `text/css` import, applied,
/// and a cross-origin `text/plain` one, a connection refused and a same-origin `nosniff` `text/plain` one throw
/// `SecurityError` on `cssRules`; this engine read the first one's rules.
pub(crate) fn import_readable(st: &OpState, href: &str) -> bool {
    same_origin(href, &crate::core::dom::document_url_of(st)) && st.try_borrow::<SheetLoads>().is_none_or(|l| !l.import_unclean.contains(href))
}

/// [`import_state`]'s answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ImportState {
    Pending,
    Failed,
    Done,
}

/// `<style>` `node` (whose sheet is `handle`) fires its `load` or `error` once its imports settle; `token` is the
/// load-delay token the caller took for it and the waker releases ([`wake_style_waiters`]).
pub(crate) fn wait_for_imports(scope: &mut v8::PinScope, node: u32, token: u32) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    with_loads(&mut st, |l| l.style_waiters.push((node, token)));
}

/// Request every `@import` the sheets now have that has not been ([`scan`]).
pub(crate) fn request_imports(scope: &mut v8::PinScope) {
    scan(scope);
}

/// An import settled: each waiting `<style>` whose imports all have fires now.
fn wake_style_waiters(scope: &mut v8::PinScope) {
    let waiters = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        with_loads(&mut st, |l| std::mem::take(&mut l.style_waiters))
    };
    let mut still = Vec::new();
    for (node, token) in waiters {
        if !crate::core::html::subresource::sheet_load_settled(scope, node, token) {
            still.push((node, token));
        }
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    with_loads(&mut st, |l| l.style_waiters.extend(still));
}

/// An import of `url` failed or was refused ([`import_state`]).
fn import_failed(scope: &mut v8::PinScope, url: &str) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    with_loads(&mut st, |l| l.import_failed.insert(url.to_string()));
}

/// A sheet's rules were just replaced from its text: put back what its `@import`s already brought.
pub(crate) fn sheet_reparsed(scope: &mut v8::PinScope) {
    scan(scope);
}

/// Page handles emptied to be given to other sheets (`style_sheet`'s copies): what was requested or is awaited under
/// them was for rules they no longer hold.
pub(crate) fn forget_sheets(st: &mut OpState, handles: &[u32]) {
    let Some(l) = st.try_borrow_mut::<SheetLoads>() else { return };
    l.imports.retain(|(h, _, _)| !handles.contains(h));
    for w in l.import_waiters.values_mut() {
        w.retain(|(h, _)| !handles.contains(h));
    }
    for h in handles {
        l.attr_style_sheets.remove(h);
    }
}

/// Every `@import` rule of every sheet whose sheet has not arrived, with its resolved URL. An import
/// nested inside an imported sheet resolves against THAT sheet's URL.
///
/// An import of a URL already on its own ancestor chain is a CYCLE and is never loaded: CSS Cascade 4
/// §2.1 has such an `@import` import nothing, and without the check `a.css` importing itself would
/// attach forever.
///
/// Each with whether it is ORIGIN-DIRTY ([`dirty_bases`]): its rule is in a sheet that is not origin-clean, or in one
/// an import took from another origin -- Blink's `StyleRuleImport` asks with `SetFromOriginDirtyStyleSheet`.
fn pending_imports(p: &page::Page, doc_base: &str, doc_url: &str) -> Vec<(u32, Vec<u32>, String, bool)> {
    let mut out = Vec::new();
    for handle in 0..p.sheet_count() as u32 {
        p.with_sheet(handle, |sheet| {
            sheet_imports(sheet, doc_base, doc_url, &mut |path, url, dirty, cyclic| {
                if network_url(&url) && !cyclic {
                    out.push((handle, path.to_vec(), url, dirty));
                }
            });
        });
    }
    out
}

/// An http(s) URL: what an `@import` here loads.
fn network_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// Every `@import` rule of `sheet` whose sheet has not arrived, nested ones included ([`pending_imports`]): `visit`
/// has its rule path, its URL resolved against the sheet that holds it, whether it is origin-dirty, and whether that
/// URL is on its own ancestor chain (a cycle).
fn sheet_imports(sheet: &Stylesheet, doc_base: &str, doc_url: &str, visit: &mut dyn FnMut(&[u32], String, bool, bool)) {
    #[allow(clippy::too_many_arguments)]
    fn walk(sheet: &Stylesheet, rules: &[page::style::rules::Rule], prefix: &mut Vec<u32>, base: &str, dirty: bool, doc_url: &str, chain: &mut Vec<String>, visit: &mut dyn FnMut(&[u32], String, bool, bool)) {
        for (i, rule) in rules.iter().enumerate() {
            prefix.push(i as u32);
            match &rule.kind {
                RuleKind::Import(import) if !import.loaded => {
                    let url = page::style::urls::resolve(base, &import.href);
                    let cyclic = chain.contains(&url);
                    visit(prefix, url, dirty, cyclic);
                }
                RuleKind::Import(import) => {
                    let child_base = sheet.base_of(import.base).unwrap_or(base).to_string();
                    let child_dirty = dirty || !same_origin(&child_base, doc_url);
                    chain.push(child_base.clone());
                    walk(sheet, &rule.children, prefix, &child_base, child_dirty, doc_url, chain, visit);
                    chain.pop();
                }
                _ => {}
            }
            prefix.pop();
        }
    }
    // A sheet with no `@import` rule -- nearly all -- has nothing to walk, and no base URL to copy.
    if !sheet.rules().iter().any(|r| matches!(r.kind, RuleKind::Import(_))) {
        return;
    }
    let base = sheet.base_url.as_deref().unwrap_or(doc_base);
    let mut chain: Vec<String> = sheet.base_url.iter().cloned().collect();
    walk(sheet, sheet.rules(), &mut Vec::new(), base, !sheet.origin_clean, doc_url, &mut chain, visit);
}

/// THE ORIGIN-DIRTY RULES OF `sheet`, by base ([`Stylesheet::base_of`]: 0 the sheet's own, then each
/// `@import`ed sheet's): all of a sheet that is not origin-clean (a cross-origin `<link>` without CORS), and an imported
/// sheet another origin than the document's served -- an import is a no-cors request -- with everything it imports.
/// What such a rule requests (an image, a font, an `@import`) Blink asks with `SetFromOriginDirtyStyleSheet`
/// (css_image_value.cc, css_font_face_src_value.cc, style_rule_import.cc), and `PopulateAndAddResourceTimingInfo`
/// adds no entry for it (resource_fetcher.cc 3222). korvus listed them: WPT resource-timing
/// no-entries-for-cross-origin-css-fetched{,-memory-cache}.
pub(crate) fn dirty_bases(sheet: &Stylesheet, doc_url: &str) -> HashSet<u32> {
    fn walk(sheet: &Stylesheet, rules: &[page::style::rules::Rule], dirty: bool, doc_url: &str, out: &mut HashSet<u32>) {
        for rule in rules {
            if let RuleKind::Import(import) = &rule.kind {
                if !import.loaded {
                    continue;
                }
                let d = dirty || sheet.base_of(import.base).is_some_and(|b| !same_origin(b, doc_url));
                if d {
                    out.insert(import.base);
                }
                walk(sheet, &rule.children, d, doc_url, out);
            }
        }
    }
    let mut out = HashSet::new();
    if !sheet.origin_clean {
        out.insert(0);
    }
    walk(sheet, sheet.rules(), !sheet.origin_clean, doc_url, &mut out);
    out
}

/// The image URLs only origin-dirty rules name ([`dirty_bases`]). A computed value no longer says which sheet it came
/// from; Blink's `CSSUrlData` does. A URL a clean rule names too is taken as clean.
fn origin_dirty_urls(p: &page::Page, doc_base: &str, doc_url: &str) -> HashSet<String> {
    let mut dirty = HashSet::new();
    let mut clean = HashSet::new();
    let bases: Vec<HashSet<u32>> = (0..p.sheet_count() as u32).map(|h| p.with_sheet(h, |sheet| dirty_bases(sheet, doc_url)).unwrap_or_default()).collect();
    if bases.iter().all(HashSet::is_empty) {
        return dirty;
    }
    for (handle, bases) in bases.iter().enumerate() {
        p.with_sheet(handle as u32, |sheet| {
            for rule in sheet.cascade_rules() {
                let base = sheet.base_of(rule.base).unwrap_or(doc_base);
                let to = if bases.contains(&rule.base) { &mut dirty } else { &mut clean };
                for decl in sheet.declarations_of(rule) {
                    let value = sheet.value_of(decl);
                    for raw in page::style::urls::urls_in(value) {
                        to.insert(page::style::urls::resolve(doc_base, &page::style::urls::resolve(base, &raw)));
                    }
                }
            }
        });
    }
    dirty.retain(|u| !clean.contains(u));
    dirty
}

/// How one property's `url()`s load -- Chrome 150's `ElementStyleResources`
/// (`core/css/resolver/element_style_resources.cc`). Measured on stock Chrome 150.0.7871.46 over a loopback access
/// log (the fixture is `tests/css_url_loads_live.rs`):
///
/// ```text
/// property                                   url(#id) / the document's own URL   another document's URL
/// background-image, border-image-source,     fetched (the DOCUMENT, as an image) image, no-cors
///   list-style-image, content, cursor,
///   -webkit-mask-box-image-source,
///   -webkit-box-reflect
/// shape-outside                              fetched (the document), cors        image, cors
/// mask-image, -webkit-mask-image, mask=""    NOT fetched (the tree scope's       image, cors
///                                            element, LoadMaskSource :374-391)
/// clip-path, fill, stroke, marker-*          NOT fetched (GetSVGResourceFromValue image, cors (the SVG document,
///                                            :281-289)                            ExternalSVGResourceDocumentContent)
/// filter, backdrop-filter                    NOT fetched                          image, SAME-ORIGIN (LoadResourcesForFilter
///                                                                                 :297-309); another origin's is never sent
/// ```
///
/// "Local" is Blink's `CSSUrlData::IsLocal` (`css_url_data.cc:225`): written fragment-only, or resolving to the
/// document's own URL but for the fragment. a production site's star ratings carry 270 `mask="url(#…)"`; fetched as
/// images they were ~277 requests for the page's own URL per visit, and a bot sensor answered 276 of them 403.
#[derive(Clone, Copy, PartialEq)]
enum UrlLoad {
    /// `StyleImageLoader::Load`, no `crossorigin`: every `url()` is fetched.
    Image,
    /// `StyleImageLoader::Load` with `kCrossOriginAttributeAnonymous`: every `url()`, as CORS.
    CorsImage,
    /// `LoadMaskSource`: a local `url()` names an element; any other is a CORS image.
    MaskSource,
    /// `SVGResource::Load(document, kCrossOriginAttributeAnonymous)`: a local `url()` names an element; any
    /// other is its document, fetched as a CORS image.
    SvgResource,
    /// `LoadResourcesForFilter`: `kCrossOriginAttributeNotSet`, so mode `same-origin`.
    SvgFilter,
}

/// The properties whose `url()`s an element's own style loads, in `LoadPendingImages` /
/// `LoadPendingSVGResources` order. `list-style-image` is its own case below: only a list item draws a marker.
const ELEMENT_LOADS: [(PropertyId, UrlLoad); 16] = [
    (PropertyId::BackgroundImage, UrlLoad::Image),
    (PropertyId::Content, UrlLoad::Image),
    (PropertyId::Cursor, UrlLoad::Image),
    (PropertyId::BorderImageSource, UrlLoad::Image),
    (PropertyId::WebkitBoxReflect, UrlLoad::Image),
    (PropertyId::WebkitMaskBoxImageSource, UrlLoad::Image),
    (PropertyId::MaskImage, UrlLoad::MaskSource),
    (PropertyId::ShapeOutside, UrlLoad::CorsImage),
    (PropertyId::BackdropFilter, UrlLoad::SvgFilter),
    (PropertyId::Filter, UrlLoad::SvgFilter),
    (PropertyId::ClipPath, UrlLoad::SvgResource),
    (PropertyId::Fill, UrlLoad::SvgResource),
    (PropertyId::MarkerEnd, UrlLoad::SvgResource),
    (PropertyId::MarkerMid, UrlLoad::SvgResource),
    (PropertyId::MarkerStart, UrlLoad::SvgResource),
    (PropertyId::Stroke, UrlLoad::SvgResource),
];

/// What a generated `::before`/`::after` box loads.
const PSEUDO_LOADS: [(PropertyId, UrlLoad); 4] = [
    (PropertyId::BackgroundImage, UrlLoad::Image),
    (PropertyId::BorderImageSource, UrlLoad::Image),
    (PropertyId::MaskImage, UrlLoad::MaskSource),
    (PropertyId::Content, UrlLoad::Image),
];

/// The request `raw` -- one `url()` of a property loaded as `how` -- induces: `(absolute URL, mode)`, or `None`
/// when it is local to the document (an element, never fetched) or a mode `same-origin` request for another
/// origin (Fetch's main fetch fails it before anything is sent: Chrome 150 never asked for a cross-origin
/// `filter: url(…)`).
fn url_request(raw: &str, how: UrlLoad, doc_base: &str, doc_url: &str) -> Option<(String, &'static str)> {
    let url = page::style::urls::resolve(doc_base, raw);
    let local = || raw.trim_start().starts_with('#') || same_ignoring_fragment(&url, doc_url);
    match how {
        UrlLoad::Image => Some((url, "no-cors")),
        UrlLoad::CorsImage => Some((url, "cors")),
        UrlLoad::MaskSource | UrlLoad::SvgResource => (!local()).then_some((url, "cors")),
        UrlLoad::SvgFilter => (!local() && same_origin(&url, doc_url)).then_some((url, "same-origin")),
    }
}

/// Blink's `EqualIgnoringFragmentIdentifier`: the two URLs, parsed, are one but for their fragments.
fn same_ignoring_fragment(a: &str, b: &str) -> bool {
    match (url::Url::parse(a), url::Url::parse(b)) {
        (Ok(mut a), Ok(mut b)) => {
            a.set_fragment(None);
            b.set_fragment(None);
            a == b
        }
        _ => false,
    }
}

fn same_origin(a: &str, b: &str) -> bool {
    match (url::Url::parse(a), url::Url::parse(b)) {
        (Ok(a), Ok(b)) => a.origin() == b.origin(),
        _ => false,
    }
}

/// The image and font loads the document's rendered elements induce right now -- and the `@font-face` rules their
/// text needs, as `(sheet, rule)` (the rule's index among its sheet's `font_faces()`), local faces included.
fn induced_loads(p: &page::Page, dom: &korvus_dom::Dom, media: &page::style::media::MediaContext, doc_base: &str, doc_url: &str) -> (Vec<Load>, Vec<(u32, u32)>) {
    let faces = font_faces(p, media, doc_base, doc_url);
    // Whether any rule styles each generated box, asked once: a lock and a matcher check per element per pseudo
    // otherwise, and the matcher cannot change while the walk below runs (the sheets are synced, the DOM is still).
    let pseudo_styled = [("before", p.styles_pseudo("before")), ("after", p.styles_pseudo("after"))];
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut used: Vec<(u32, u32)> = Vec::new();
    p.rendered_styles(dom, media, |node, value| {
        let display = value(PropertyId::Display);
        if display == "none" {
            return false;
        }
        for (property, how) in ELEMENT_LOADS {
            for raw in image_urls(&value(property)) {
                if let Some((url, mode)) = url_request(&raw, how, doc_base, doc_url) {
                    push_load(&mut out, &mut seen, url, "image", mode, false);
                }
            }
        }
        if display.contains("list-item") {
            for url in image_urls(&value(PropertyId::ListStyleImage)) {
                push_load(&mut out, &mut seen, page::style::urls::resolve(doc_base, &url), "image", "no-cors", false);
            }
        }
        if !faces.is_empty() {
            if let Some(text) = rendered_text(dom, node) {
                for face in used_face_refs(&faces, &value(PropertyId::FontFamily), &value(PropertyId::FontWeight), &value(PropertyId::FontStyle), &text) {
                    if !used.contains(&face.key) {
                        used.push(face.key);
                    }
                    if let Some(url) = face.url.clone() {
                        push_load(&mut out, &mut seen, url, "font", "cors", face.dirty);
                    }
                }
            }
        }
        // The element's generated boxes load what their own styles name: a
        // `::before`/`::after` that generates a box -- its `content` is not `none` and its `display`
        // is not `none` -- exactly as an element would.
        for (pseudo, styled) in pseudo_styled {
            if !styled {
                continue;
            }
            let pv = |property: PropertyId| p.computed_pseudo_value(dom, node, pseudo, media, property, None).unwrap_or_default();
            let content = pv(PropertyId::Content);
            if content == "none" || content == "normal" || pv(PropertyId::Display) == "none" {
                continue;
            }
            for (property, how) in PSEUDO_LOADS {
                for raw in image_urls(&pv(property)) {
                    if let Some((url, mode)) = url_request(&raw, how, doc_base, doc_url) {
                        push_load(&mut out, &mut seen, url, "image", mode, false);
                    }
                }
            }
            if !faces.is_empty() {
                let text: String = page::style::generated::content_items(&content)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|i| match i {
                        page::style::generated::ContentItem::Text(t) => Some(t),
                        _ => None,
                    })
                    .collect();
                if !collapses_to_nothing(&text) {
                    for face in used_face_refs(&faces, &pv(PropertyId::FontFamily), &pv(PropertyId::FontWeight), &pv(PropertyId::FontStyle), &text) {
                        if !used.contains(&face.key) {
                            used.push(face.key);
                        }
                        if let Some(url) = face.url.clone() {
                            push_load(&mut out, &mut seen, url, "font", "cors", face.dirty);
                        }
                    }
                }
            }
        }
        true
    });
    (out, used)
}

/// An image's `dirty` is decided later, for the loads a scan starts ([`origin_dirty_urls`]); a font's is its face's.
fn push_load(out: &mut Vec<Load>, seen: &mut HashSet<String>, url: String, destination: &'static str, mode: &'static str, dirty: bool) {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return; // `data:` and `blob:` never reach the network
    }
    if seen.insert(url.clone()) {
        out.push(Load { url, destination, mode, dirty });
    } else if !dirty {
        // a clean face naming it too: the request is not from an origin-dirty sheet
        if let Some(l) = out.iter_mut().find(|l| l.url == url) {
            l.dirty = false;
        }
    }
}

/// The URLs an image-valued property requests: every `url()`, except inside `image-set()`, where only
/// the first candidate is taken -- this persona's device pixel ratio is 1, and Chrome requested the
/// `1x` candidate alone.
fn image_urls(value: &str) -> Vec<String> {
    if !value.contains("url(") {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut rest = value;
    loop {
        let set = rest.find("image-set(").or_else(|| rest.find("-webkit-image-set("));
        let url = rest.find("url(");
        match (set, url) {
            (Some(s), Some(u)) if s < u => {
                // Take the first url() inside the set, skip to the set's closing paren.
                let inner = &rest[s..];
                let mut depth = 0;
                let mut end = inner.len();
                for (i, c) in inner.char_indices() {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = i + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(first) = page::style::urls::urls_in(&inner[..end]).into_iter().next() {
                    out.push(first);
                }
                rest = &inner[end..];
            }
            (_, Some(u)) => {
                let inner = &rest[u..];
                let end = inner.find(')').map_or(inner.len(), |e| e + 1);
                out.extend(page::style::urls::urls_in(&inner[..end]));
                rest = &inner[end..];
            }
            _ => break,
        }
    }
    out
}

/// Does `text` collapse away to nothing? CSS white space processing collapses the ASCII white space
/// characters and drops a run of them at the edges of a line, so an element holding only those
/// renders no glyph and needs no face. `char::is_whitespace` is the WRONG test: it is the Unicode
/// `White_Space` property, which includes U+00A0 NO-BREAK SPACE -- and a NO-BREAK SPACE does not
/// collapse. Chrome 150 fetches the face for `<div class=nbsp>&nbsp;</div>` and not for
/// `<div class=sp>   </div>` (`e21b.html`).
fn collapses_to_nothing(text: &str) -> bool {
    text.chars().all(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}'))
}

/// `text` with the collapsible white space at its edges removed -- the characters that actually
/// reach a glyph, which is what decides which faces load.
fn rendered_chars(text: &str) -> &str {
    text.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}'))
}

/// The text an element renders with its OWN font: its non-whitespace text children, or a form
/// control's value or placeholder. `None` when it renders none.
fn rendered_text(dom: &korvus_dom::Dom, node: u32) -> Option<String> {
    let tag = dom.tag_name(node)?.to_ascii_lowercase();
    if matches!(tag.as_str(), "input" | "textarea") {
        let v = dom.control_value(node);
        let v = if v.is_empty() { dom.get_attribute(node, "placeholder").unwrap_or("").to_string() } else { v };
        let text = if tag == "textarea" { dom.text_content(node) } else { v.to_string() };
        let text = if collapses_to_nothing(&text) { v.to_string() } else { text };
        return (!collapses_to_nothing(&text)).then_some(text);
    }
    let mut text = String::new();
    for child in dom.children(node) {
        if dom.node_type(child) == 3 {
            text.push_str(&dom.text_content(child));
        }
    }
    (!collapses_to_nothing(&text)).then_some(text)
}

// ── @font-face selection ─────────────────────────────────────────────────────────────────────────

/// One applicable `@font-face`: the family it declares, its style and weight range, its
/// unicode-range, and the URL its `src` would load.
struct Face {
    family: String,
    style: String,
    weight: (f32, f32),
    ranges: Vec<(u32, u32)>,
    url: Option<String>,
    /// `(sheet, rule)`: the sheet's handle and the rule's index among its `font_faces()` -- the identity
    /// `core/fonts/font_face.rs` knows the CSS-connected `FontFace` of this rule by.
    key: (u32, u32),
    /// The rule is origin-dirty ([`dirty_bases`]): its font's request makes no resource-timing entry.
    dirty: bool,
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches(|c| c == '"' || c == '\'').trim().to_string()
}

fn font_faces(p: &page::Page, media: &page::style::media::MediaContext, doc_base: &str, doc_url: &str) -> Vec<Face> {
    let mut out = Vec::new();
    for handle in 0..p.sheet_count() as u32 {
        p.with_sheet(handle, |sheet| {
            if !sheet.sheet_applies(media) {
                return;
            }
            let dirty = if sheet.font_faces().is_empty() { HashSet::new() } else { dirty_bases(sheet, doc_url) };
            for (index, entry) in sheet.font_faces().iter().enumerate() {
                if !sheet.chain_matches(entry.media, media) {
                    continue;
                }
                let family = unquote(entry.block.get("font-family")).to_ascii_lowercase();
                if family.is_empty() {
                    continue;
                }
                let base = sheet.base_of(entry.base).unwrap_or(doc_base).to_string();
                out.push(Face {
                    family,
                    style: entry.block.get("font-style").trim().to_ascii_lowercase(),
                    weight: weight_range(entry.block.get("font-weight")),
                    ranges: unicode_ranges(entry.block.get("unicode-range")),
                    url: face_src(entry.block.get("src")).map(|u| page::style::urls::resolve(&base, &u)),
                    key: (handle, index as u32),
                    dirty: dirty.contains(&entry.base),
                });
            }
        });
    }
    out
}

/// `font-weight` descriptor: `normal`, `bold`, a number, or a `lo hi` range.
fn weight_range(text: &str) -> (f32, f32) {
    let one = |t: &str| -> f32 { match t.trim().to_ascii_lowercase().as_str() {
        "" | "normal" | "auto" => 400.0,
        "bold" => 700.0,
        n => n.parse().unwrap_or(400.0),
    } };
    let parts: Vec<&str> = text.split_ascii_whitespace().collect();
    match parts.as_slice() {
        [a, b] => (one(a).min(one(b)), one(a).max(one(b))),
        [a] => (one(a), one(a)),
        _ => (400.0, 400.0),
    }
}

/// `unicode-range: U+0-7F, U+0400-04FF, U+4??` as inclusive code point ranges. Empty = everything.
fn unicode_ranges(text: &str) -> Vec<(u32, u32)> {
    text.split(',')
        .filter_map(|part| {
            let t = part.trim();
            let t = t.strip_prefix("U+").or_else(|| t.strip_prefix("u+"))?;
            if let Some((a, b)) = t.split_once('-') {
                return Some((u32::from_str_radix(a, 16).ok()?, u32::from_str_radix(b, 16).ok()?));
            }
            if t.contains('?') {
                let lo = u32::from_str_radix(&t.replace('?', "0"), 16).ok()?;
                let hi = u32::from_str_radix(&t.replace('?', "F"), 16).ok()?;
                return Some((lo, hi));
            }
            let v = u32::from_str_radix(t, 16).ok()?;
            Some((v, v))
        })
        .collect()
}

/// The first `src` entry this engine would load: a `url()` whose `format()` (if any) is one Chrome
/// accepts. `local()` is skipped -- no persona reports installed faces -- and so is
/// `embedded-opentype` / `svg`, which Chrome passed over for the next entry (measured).
fn face_src(src: &str) -> Option<String> {
    let mut depth = 0;
    let mut start = 0;
    let mut parts = Vec::new();
    for (i, c) in src.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&src[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&src[start..]);
    for part in parts {
        let lower = part.to_ascii_lowercase();
        if !lower.contains("url(") {
            continue;
        }
        if let Some(f) = lower.find("format(") {
            let fmt = unquote(lower[f + 7..].split(')').next().unwrap_or(""));
            if !matches!(fmt.as_str(), "woff2" | "woff" | "truetype" | "opentype" | "collection" | "woff2-variations" | "woff-variations" | "truetype-variations" | "opentype-variations") {
                continue;
            }
        }
        if let Some(url) = page::style::urls::urls_in(part).into_iter().next() {
            return Some(url);
        }
    }
    None
}

/// The generic families: always available, and the end of a family list's search.
fn generic(family: &str) -> bool {
    matches!(
        family,
        "serif" | "sans-serif" | "monospace" | "cursive" | "fantasy" | "system-ui" | "math" | "emoji"
            | "fangsong" | "ui-serif" | "ui-sans-serif" | "ui-monospace" | "ui-rounded"
            | "-webkit-body" | "-webkit-standard"
    )
}

/// The faces an element's text loads: CSS Fonts 4 §5, run PER CHARACTER. For each character of the
/// rendered text, walk the family list; in the first family that declares a face whose
/// unicode-range covers THAT character, the best match for style and weight is USED and loaded; a
/// generic or installed family ends that character's search first.
///
/// A family is more than one file, which is the whole point of `unicode-range`: Chrome 150 on four
/// faces of one family with the ranges `U+0000-00FF` / `U+0100-024F` / `U+0400-045F` / `U+4E00-9FFF`
/// and the text `Latin ABC Ā Ё` fetches the first three and not the fourth. Matching once against
/// the whole string -- which this once did -- fetched only the latin face, and a
/// family list `Chain1, Chain2` whose first family covers only the latin characters never reached
/// `Chain2` for the cyrillic one.
///
/// The faces themselves, not only the URLs to fetch: a face with no `url()` this engine would load (a `local()` one)
/// is used all the same -- it ends the character's search -- and is the document's CSS-connected `FontFace` to load.
fn used_face_refs<'a>(faces: &'a [Face], families: &str, weight: &str, style: &str, text: &str) -> Vec<&'a Face> {
    let want_weight: f32 = weight.trim().parse().unwrap_or(400.0);
    let want_italic = style.trim() != "normal";
    let list: Vec<String> = families.split(',').map(|f| unquote(f).to_ascii_lowercase()).filter(|f| !f.is_empty()).collect();
    let mut out: Vec<&'a Face> = Vec::new();
    // The same character twice asks the same question, and a paragraph is mostly repeats. A SET and
    // not a `Vec::contains`: this runs per rendered element on every scan, and a page of CJK can
    // carry a thousand distinct characters in one element -- which a linear scan makes quadratic.
    let mut asked: HashSet<char> = HashSet::new();
    for c in rendered_chars(text).chars() {
        if !asked.insert(c) {
            continue;
        }
        for family in &list {
            let candidates: Vec<&'a Face> = faces
                .iter()
                .filter(|f| &f.family == family)
                .filter(|f| f.ranges.is_empty() || f.ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&(c as u32))))
                .collect();
            if !candidates.is_empty() {
                if let Some(face) = best_match(&candidates, want_weight, want_italic) {
                    if !out.iter().any(|f| f.key == face.key) {
                        out.push(face);
                    }
                }
                break;
            }
            // an installed family ends the character's search -- when it has the character (F9: a Noto script face
            // has the space and no Latin letter, and Blink takes a letter from the next family)
            if generic(family) || page::font::resolve(family).is_some_and(|m| m.family.eq_ignore_ascii_case(family) && m.covers(c)) {
                break;
            }
        }
    }
    out
}

/// The URLs [`used_face_refs`] loads, each once, in order.
#[cfg(test)]
fn used_faces(faces: &[Face], families: &str, weight: &str, style: &str, text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for url in used_face_refs(faces, families, weight, style, text).into_iter().filter_map(|f| f.url.clone()) {
        if !out.contains(&url) {
            out.push(url);
        }
    }
    out
}

/// CSS Fonts 4 §5.2 steps 4b/4c: style first (italic wants italic then oblique then normal; normal
/// wants normal then oblique then italic), then weight -- inside the range if one covers it; else for
/// a wanted weight in 400..=500 the next heavier up to 500, then lighter, then heavier; below 400
/// lighter first; above 500 heavier first.
///
/// A TIE goes to the face declared LAST, which is why the pool is walked in reverse: two
/// indistinguishable `@font-face` rules for one family make Chrome 150 fetch the second
/// (`e21c.html`'s `f_tie_b`), and so do two whose `unicode-range`s overlap on the character being
/// matched (`e21b.html`'s `f_ovb`). `min_by` keeps the FIRST of equal minima, so the reverse is the
/// whole fix.
fn best_match<'a>(faces: &[&'a Face], weight: f32, italic: bool) -> Option<&'a Face> {
    let style_rank = |f: &Face| -> u8 {
        let s = f.style.as_str();
        let is_italic = s.starts_with("italic");
        let is_oblique = s.starts_with("oblique");
        match (italic, is_italic, is_oblique) {
            (true, true, _) | (false, false, false) => 0,
            (_, _, true) => 1,
            _ => 2,
        }
    };
    let best_style = faces.iter().map(|f| style_rank(f)).min()?;
    let pool: Vec<&'a Face> = faces.iter().copied().rev().filter(|f| style_rank(f) == best_style).collect();
    let distance = |f: &Face| -> (u8, f32) {
        let (lo, hi) = f.weight;
        if (lo..=hi).contains(&weight) {
            return (0, 0.0);
        }
        let heavier = lo > weight;
        let gap = if heavier { lo - weight } else { weight - hi };
        let order = if (400.0..=500.0).contains(&weight) {
            if heavier && lo <= 500.0 { 1 } else if !heavier { 2 } else { 3 }
        } else if weight < 400.0 {
            if heavier { 2 } else { 1 }
        } else if heavier {
            1
        } else {
            2
        };
        (order, gap)
    };
    pool.into_iter().min_by(|a, b| distance(a).partial_cmp(&distance(b)).unwrap_or(std::cmp::Ordering::Equal))
}

/// The document's `@font-face` faces, as its painter draws text in them (`cssom::complete_fonts`).
pub(crate) struct WebFaces(Vec<Face>);

impl WebFaces {
    /// The faces the document's applicable sheets declare; `None` when they declare none.
    pub(crate) fn of(st: &OpState, media: &page::style::media::MediaContext) -> Option<WebFaces> {
        let p = page_of(st)?;
        let faces = font_faces(&p, media, &crate::core::dom::base_url_of(st), &crate::core::dom::document_url_of(st));
        (!faces.is_empty()).then_some(WebFaces(faces))
    }

    /// Each face with a URL: its family (lowercase), `font-style`, `font-weight` range, `unicode-range` and URL.
    pub(crate) fn each(&self) -> impl Iterator<Item = (&str, &str, (f32, f32), &[(u32, u32)], &str)> {
        self.0.iter().filter_map(|f| Some((f.family.as_str(), f.style.as_str(), f.weight, f.ranges.as_slice(), f.url.as_deref()?)))
    }

    /// Does an `@font-face` rule declare `family` (lowercase, unquoted)? Then no installed font answers to the name.
    pub(crate) fn declares(&self, family: &str) -> bool {
        self.0.iter().any(|f| f.family == family)
    }

    /// The URLs of the faces `family` draws text of `weight` and style in, in declaration order: the best match for
    /// each `unicode-range` the family's rules split it into ([`best_match`], CSS Fonts 4 §5.2) -- a face per subset --
    /// each with its `font-weight` range.
    pub(crate) fn urls(&self, family: &str, weight: f32, italic: bool) -> Vec<(String, (f32, f32))> {
        let mut groups: Vec<Vec<&Face>> = Vec::new();
        for face in self.0.iter().filter(|f| f.family == family) {
            match groups.iter_mut().find(|g| g[0].ranges == face.ranges) {
                Some(g) => g.push(face),
                None => groups.push(vec![face]),
            }
        }
        let mut chosen: Vec<&Face> = groups.iter().filter_map(|g| best_match(g, weight, italic)).collect();
        chosen.sort_by_key(|f| f.key);
        let mut out: Vec<(String, (f32, f32))> = Vec::new();
        for (url, range) in chosen.into_iter().filter_map(|f| Some((f.url.clone()?, f.weight))) {
            if !out.iter().any(|(u, _)| *u == url) {
                out.push((url, range));
            }
        }
        out
    }
}

// ── starting a load ──────────────────────────────────────────────────────────────────────────────

fn current_epoch(scope: &mut v8::PinScope) -> u32 {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    with_loads(&mut st, |l| l.epoch)
}

/// An image or a font: requested, the document's load event delayed until it settles, the body
/// dropped.
fn start_load(scope: &mut v8::PinScope, load: &Load) {
    // CSP: a style sheet's request is asked of the document's policies as any request is. A refused
    // image is never requested -- its violation reported, its `css` entry made now (unless origin-dirty) -- where korvus
    // put it on the wire (Chrome 150 x3, `img-src /no/bg.png`, an entry with every phase 0, no GET). A
    // refused font is not requested either; its CSS-connected face reports it when it loads (`core/fonts/font_face.rs`).
    let refused = match load.destination {
        "image" => crate::core::html::csp::realm_request_blocked(scope, &load.url, "image"),
        "font" => !crate::core::html::csp::page_request_allowed_quietly(scope, &load.url, "font"),
        _ => false,
    };
    if refused {
        if load.destination == "image" && !load.dirty {
            crate::core::timing::resource_timing::refused_request(scope, "css", &load.url);
        }
        return;
    }
    let token = new_load_delay_token(scope);
    delay_load(scope, token);
    // A load joins the request this document already has for the same URL,
    // whether an element or a sheet names it -- two `@font-face` rules pointing one `.woff2` file
    // are ONE request on Chrome 150.0.7871.46 and were two here. See `subresource::DocumentRequests`.
    crate::core::timing::resource_timing::from_origin_dirty_sheet(scope, load.dirty);
    let fetched = crate::core::html::subresource::start_shared_fetch(scope, &load.url, load.destination, load.mode);
    crate::core::timing::resource_timing::from_origin_dirty_sheet(scope, false);
    let Some(p) = fetched else {
        undelay_load(scope, token);
        return;
    };
    let data = v8::Integer::new_from_unsigned(scope, token).into();
    let done = build_fn_data(scope, cb_load_settled, "", 1, data);
    p.then2(scope, done, done);
}

fn cb_load_settled(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(token) = crate::binding::protos::body_data(scope, &args).uint32_value(scope) else { return };
    undelay_load(scope, token);
}

/// An `@import`: requested once per URL, its sheet attached to every rule that imports it.
fn start_import(scope: &mut v8::PinScope, handle: u32, path: Vec<u32>, url: String, dirty: bool) {
    let cached = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        with_loads(&mut st, |l| match l.import_bodies.get(&url) {
            Some(Some(body)) => Some(Some(body.clone())),
            Some(None) => {
                l.import_waiters.entry(url.clone()).or_default().push((handle, path.clone()));
                Some(None)
            }
            None => {
                l.import_bodies.insert(url.clone(), None);
                l.import_waiters.entry(url.clone()).or_default().push((handle, path.clone()));
                None
            }
        })
    };
    match cached {
        Some(Some((final_url, body))) => {
            if attach(scope, handle, &path, &final_url, &body) {
                scan(scope);
            }
        }
        Some(None) => {}
        None => {
            // refused by CSP (`style-src`), as a `<link>` sheet is: never requested, its entry made now
            if crate::core::html::csp::realm_request_blocked(scope, &url, "style") {
                if !dirty {
                    crate::core::timing::resource_timing::refused_request(scope, "css", &url);
                }
                import_failed(scope, &url);
                wake_style_waiters(scope);
                return;
            }
            let token = new_load_delay_token(scope);
            delay_load(scope, token);
            crate::core::timing::resource_timing::from_origin_dirty_sheet(scope, dirty);
            let fetched = crate::core::html::subresource::start_fetch(scope, &url, "style", "no-cors");
            crate::core::timing::resource_timing::from_origin_dirty_sheet(scope, false);
            let Some(p) = fetched else {
                import_failed(scope, &url);
                wake_style_waiters(scope);
                undelay_load(scope, token);
                return;
            };
            let epoch = current_epoch(scope);
            let items = [
                v8::Integer::new_from_unsigned(scope, token).into(),
                v8::Integer::new_from_unsigned(scope, epoch).into(),
                str_val(scope, &url),
            ];
            let data: v8::Local<v8::Value> = v8::Array::new_with_elements(scope, &items).into();
            let ok = build_fn_data(scope, cb_import_ok, "", 1, data);
            let err = build_fn_data(scope, cb_import_err, "", 1, data);
            // the fetch's end is a task, not a promise job, and one Chrome posts once the response is processed, as it
            // posts a `<link>` sheet's `load` (`then_new_task`)
            crate::then_new_task(scope, p, ok, Some(err));
        }
    }
}

fn import_data(scope: &mut v8::PinScope, data: v8::Local<v8::Value>) -> Option<(u32, u32, String)> {
    let arr = v8::Local::<v8::Array>::try_from(data).ok()?;
    let token = arr.get_index(scope, 0)?.uint32_value(scope)?;
    let epoch = arr.get_index(scope, 1)?.uint32_value(scope)?;
    let url = arr.get_index(scope, 2)?.to_rust_string_lossy(scope);
    Some((token, epoch, url))
}

/// Parse an imported sheet's body into the `@import` at `(handle, path)`. `false` when that rule is not
/// (or no longer) an import waiting for a sheet.
fn attach(scope: &mut v8::PinScope, handle: u32, path: &[u32], final_url: &str, body: &str) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let Some(p) = page_of(&st) else { return false };
    let child = page::style::parse::parse_stylesheet(page::style::rules::Origin::Author, body);
    p.with_sheet_mut(handle, |s| s.attach_import(path, final_url, child)).unwrap_or(false)
}

/// An import of `url` was answered from `final_url`: every rule waiting for it gets its sheet -- `body` parsed, empty
/// for an answer that is not CSS -- and the `<style>`s and links waiting for their imports are told. `failed` is an
/// answer their `load` is an `error` for, `unclean` one whose sheet script may not read ([`import_readable`]).
fn settle_import(scope: &mut v8::PinScope, url: &str, final_url: &str, body: String, failed: bool, unclean: bool) {
    let waiters = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        with_loads(&mut st, |l| {
            if failed {
                l.import_failed.insert(url.to_string());
            }
            if unclean {
                l.import_unclean.insert(final_url.to_string());
            }
            l.import_bodies.insert(url.to_string(), Some((final_url.to_string(), body.clone())));
            l.import_waiters.remove(url).unwrap_or_default()
        })
    };
    for (handle, path) in waiters {
        attach(scope, handle, &path, final_url, &body);
    }
    // The imported sheet can import in turn, and its rules can induce loads: before the release that follows, so the
    // document keeps waiting for them.
    if !body.is_empty() {
        scan(scope);
    }
    wake_style_waiters(scope);
}

/// An import's response. Chrome 152.0.7977.82, 2026-10-23, an `@import` in a `<style>` and in a linked sheet, a
/// no-quirks and a quirks document:
///
/// ```text
///   response                                   styleSheet.cssRules   applied           the importer's event
///   200 text/css, none, empty type             its rules             yes               load
///   200 text/plain, text/html, octet-stream    0 rules               quirks only       load
///   200 text/plain from another origin         SecurityError         no                load
///   200 text/plain + nosniff                   SecurityError         no                error
///   404, 500                                   0 rules               no                error
///   200 text/css from another origin           SecurityError         yes               load
///   the connection refused                     SecurityError         no                error  (cb_import_err)
/// ```
///
/// This engine refused every type but CSS as a failure -- `error` at the importer, which a linked sheet now waits for
/// (a loadCSS `onload` promise rejected) -- and gave every failed import a `styleSheet` of null and no quirks pass.
/// "Applied in quirks" is the same test a `<link>`'s own bytes take ([`crate::core::html::subresource::stylesheet_mime_ok`]
/// and the CORS-same-origin answer of `stylesheet_arrived`).
fn cb_import_ok(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((token, epoch, url)) = import_data(scope, __body_data) else { return };
    if epoch == current_epoch(scope) {
        let resp = v8::Local::<v8::Object>::try_from(args.get(0)).ok();
        let status = resp.and_then(|r| get_prop(scope, r, "status")).and_then(|s| s.int32_value(scope)).unwrap_or(0);
        let headers = resp.and_then(|r| get_prop(scope, r, "headers")).map(|h| headers_from_js(scope, h)).unwrap_or_default();
        let final_url = resp
            .and_then(|r| get_prop(scope, r, "url"))
            .filter(|u| u.boolean_value(scope))
            .map(|u| u.to_rust_string_lossy(scope))
            .unwrap_or_else(|| url.clone());
        let ok = (200..300).contains(&status);
        let css = crate::core::html::subresource::stylesheet_mime_ok(&headers);
        let refused = !css && transport::mime::nosniff(&headers);
        // Not CSS, not refused: parsed all the same in a quirks document, for a response of its own origin.
        let quirks_css = ok && !css && !refused && {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            let quirks = st.try_borrow::<HostDom>().is_some_and(|h| h.0.document_mode(h.0.root()) == korvus_dom::DocumentMode::Quirks);
            quirks && same_origin(&final_url, &crate::core::dom::document_url_of(&st))
        };
        let body = if ok && (css || quirks_css) {
            resp.and_then(|r| get_prop(scope, r, "body")).filter(|b| !b.is_null_or_undefined()).map(|b| b.to_rust_string_lossy(scope)).unwrap_or_default()
        } else {
            String::new()
        };
        settle_import(scope, &url, &final_url, body, !ok || refused, refused);
    }
    undelay_load(scope, token);
}

fn cb_import_err(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let __body_data = crate::binding::protos::body_data(scope, &args);
    let Some((token, epoch, url)) = import_data(scope, __body_data) else { return };
    if epoch == current_epoch(scope) {
        // No response: an empty sheet all the same, unreadable, and the importer's `error` ([`cb_import_ok`]).
        settle_import(scope, &url, &url, String::new(), true, true);
    }
    undelay_load(scope, token);
}

// ── the rendering-opportunity frame ──────────────────────────────────────────────────────────────

/// Arm the frame, once per document. It re-arms itself for as long as the realm lives, and its
/// promise is UNREF'd so it never keeps the event loop alive on its own.
fn arm_frame(scope: &mut v8::PinScope) {
    let epoch = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let armed = with_loads(&mut st, |l| std::mem::replace(&mut l.frame_armed, true));
        if armed {
            return;
        }
        with_loads(&mut st, |l| l.epoch)
    };
    frame_after(scope, epoch);
}

fn frame_after(scope: &mut v8::PinScope, epoch: u32) {
    let Some(f) = crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_css_frame") else { return };
    let ms: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, FRAME_MS).into();
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let Some(p) = f.call(scope, undef, &[ms]).and_then(|v| v8::Local::<v8::Promise>::try_from(v).ok()) else { return };
    crate::core::workers::worker::unref(scope, p.into());
    let data: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, epoch).into();
    let tick = build_fn_data(scope, cb_frame, "", 1, data);
    p.then2(scope, tick, tick);
}

fn cb_frame(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(epoch) = crate::binding::protos::body_data(scope, &args).uint32_value(scope) else { return };
    if epoch != current_epoch(scope) {
        // The document this frame belonged to is gone; the new one arms its own.
        return;
    }
    let changed = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        let media = cssom::media_context(&st);
        // What a scan reads moves its key ([`scan_key`]). The cascade fingerprint and the arena's length it replaces
        // also moved with changes to nodes no document holds, no-op attribute writes and transform/opacity-only
        // `style` rewrites, none of which can induce a load unless generated content shows the `style` text:
        // React's render phases rescanned the whole document every 100 ms for them.
        let now = match (page_of(&st), st.try_borrow::<HostDom>()) {
            (Some(p), Some(host)) => {
                p.sync_document_sheets(&host.0);
                Some((p.page_geometry_fingerprint(&host.0, &media), host.0.inert_style_writes(), p))
            }
            _ => None,
        };
        with_loads(&mut st, |l| {
            let key = now.map(|(fingerprint, inert, p)| scan_key(l, &p, fingerprint, inert));
            key.is_some() && l.last != key
        })
    };
    if changed {
        scan(scope);
    }
    frame_after(scope, epoch);
}

/// The frame's clock: a sleep the scan's continuation waits on. Not a page timer -- a page that
/// replaces `setTimeout` cannot see it -- and its promise is unref'd by [`frame_after`].
#[op2(async(lazy), fast)]
pub(crate) async fn op_css_frame(#[smi] ms: u32) {
    tokio::time::sleep(std::time::Duration::from_millis(u64::from(ms))).await;
}

crate::webapi_ops![op_css_frame];

#[cfg(test)]
mod tests {
    use super::*;

    fn face(family: &str, style: &str, weight: &str, range: &str, src: &str) -> Face {
        // each rule its own identity, as each `@font-face` of a sheet is
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        Face {
            family: family.into(),
            style: style.into(),
            weight: weight_range(weight),
            ranges: unicode_ranges(range),
            url: face_src(src),
            key: (0, NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)),
            dirty: false,
        }
    }

    // Every row is `t2.html`'s, measured on Chrome 150 (this file's header table).
    #[test]
    fn the_face_chrome_loads_is_the_face_this_picks() {
        let faces = vec![
            face("f5", "", "400", "", "url(w400)"),
            face("f5", "", "700", "", "url(w700)"),
            face("f6", "", "", "", "local(Nope), url(eot) format(\"embedded-opentype\"), url(second) format(\"woff2\"), url(third)"),
            face("f14", "italic", "", "", "url(italic)"),
            face("range", "", "", "U+0400-04FF", "url(range)"),
            face("rangeok", "", "", "U+0041-005A", "url(rangeok)"),
            face("f1", "", "", "", "url(segoe)"),
        ];
        let pick = |families: &str, text: &str| used_faces(&faces, families, "400", "normal", text);
        assert_eq!(pick("F5", "w400 only"), ["w400"]);
        assert_eq!(pick("F6", "f6"), ["second"]);
        assert_eq!(pick("F14", "x"), ["italic"], "an italic face still serves normal text");
        assert!(pick("Range", "latin").is_empty(), "outside its unicode-range");
        assert_eq!(pick("RangeOK", "UPPER"), ["rangeok"]);
        assert!(pick("sans-serif, F1", "x").is_empty(), "a generic family ends the search");
        assert_eq!(pick("'Segoe UI', F1", "x"), ["segoe"], "an uninstalled family does not");
        assert!(pick("Arial, F1", "x").is_empty(), "an installed family does");
    }

    /// Every row is stock Chrome 150 on this unit's `e21*.html`; before it, each of
    /// these answered with the FIRST matching face and nothing else.
    #[test]
    fn a_family_is_more_than_one_file_and_the_match_runs_per_character() {
        let faces = vec![
            face("multi", "", "", "U+0000-00FF", "url(latin)"),
            face("multi", "", "", "U+0100-024F", "url(latinext)"),
            face("multi", "", "", "U+0400-045F", "url(cyrillic)"),
            face("multi", "", "", "U+4E00-9FFF", "url(cjk)"),
            face("ov", "", "", "U+0041-005A", "url(ova)"),
            face("ov", "", "", "U+0041-007A", "url(ovb)"),
            face("tie", "", "", "", "url(tie_a)"),
            face("tie", "", "", "", "url(tie_b)"),
            face("chain1", "", "", "U+0041-005A", "url(c1)"),
            face("chain2", "", "", "U+0400-045F", "url(c2)"),
            face("after", "", "", "U+0400-045F", "url(after)"),
            face("wt", "", "400", "U+0000-00FF", "url(wt400latin)"),
            face("wt", "", "700", "U+0000-00FF", "url(wt700latin)"),
            face("wt", "", "400", "U+0400-045F", "url(wt400cyr)"),
            face("wt", "", "700", "U+0400-045F", "url(wt700cyr)"),
        ];
        let pick = |families: &str, text: &str| used_faces(&faces, families, "400", "normal", text);
        assert_eq!(pick("Multi", "Latin ABC \u{100} \u{401}"), ["latin", "latinext", "cyrillic"]);
        assert_eq!(pick("Multi", "\u{4E2D}"), ["cjk"], "and the CJK face when a character needs it");
        assert_eq!(pick("Ov", "Az"), ["ovb"], "overlapping ranges: the LAST declared face wins");
        assert_eq!(pick("Tie", "T"), ["tie_b"], "and so does an exact tie");
        assert_eq!(pick("Chain1, Chain2", "A \u{401}"), ["c1", "c2"], "the list is walked per character");
        assert!(pick("serif, After", "\u{401}").is_empty(), "a generic ends the search for that character");
        assert_eq!(pick("Wt", "W \u{401}"), ["wt400latin", "wt400cyr"], "the weight filter narrows inside each range");
    }

    /// White space that collapses away renders no glyph and needs no face -- but U+00A0 does NOT
    /// collapse, and `char::is_whitespace` says it does. Chrome 150 fetches the face for an element
    /// holding only `&nbsp;` and not for one holding only spaces.
    #[test]
    fn only_collapsible_white_space_renders_nothing() {
        assert!(collapses_to_nothing("   \t\n\r"));
        assert!(collapses_to_nothing(""));
        assert!(!collapses_to_nothing("\u{a0}"), "a no-break space renders");
        assert!(!collapses_to_nothing(" x "));
        assert_eq!(rendered_chars("  A \u{401}  "), "A \u{401}");
        assert_eq!(rendered_chars("\u{a0}"), "\u{a0}");
    }

    /// Blink's `IsLocal`: fragment-only, or the document's own URL but for the fragment -- then a mask or an SVG
    /// resource names an element and nothing is fetched, while an image property still fetches the document.
    #[test]
    fn a_local_url_is_an_element_for_a_mask_and_a_fetch_for_an_image() {
        let (base, doc) = ("http://h:1/v/p?q=1", "http://h:1/v/p?q=1");
        let req = |raw: &str, how| url_request(raw, how, base, doc);
        assert_eq!(req("#m", UrlLoad::MaskSource), None);
        assert_eq!(req("p?q=1#m", UrlLoad::MaskSource), None, "the document's own URL, relatively");
        assert_eq!(req("http://h:1/v/p?q=1#m", UrlLoad::SvgResource), None);
        assert_eq!(req("p#m", UrlLoad::MaskSource), Some(("http://h:1/v/p#m".to_string(), "cors")), "another query is another document");
        assert_eq!(req("#bg", UrlLoad::Image), Some(("http://h:1/v/p?q=1#bg".to_string(), "no-cors")));
        assert_eq!(req("#s", UrlLoad::CorsImage), Some(("http://h:1/v/p?q=1#s".to_string(), "cors")));
        assert_eq!(req("/x.svg#c", UrlLoad::SvgResource), Some(("http://h:1/x.svg#c".to_string(), "cors")));
        assert_eq!(req("/x.svg#f", UrlLoad::SvgFilter), Some(("http://h:1/x.svg#f".to_string(), "same-origin")));
        assert_eq!(req("http://o:2/x.svg#f", UrlLoad::SvgFilter), None, "a same-origin request for another origin is never sent");
    }

    /// [`scan_key`] counts inert `style` rewrites only for a sheet that shows the attribute's text.
    #[test]
    fn only_attr_of_style_is_the_style_attributes_text() {
        for yes in ["content:attr(style)", "content: ATTR( Style )", "content: attr(/* c */style)", "content:attr(style string)", "--t: attr(style)", "content: attr(\\73 tyle)", "content: attr(*|style)", "content: attr(st\\79le)"] {
            assert!(names_attr_style(yes), "{yes}");
        }
        for no in ["content:attr(data-x)", "content: attr(href) \" \"", "content: myattr(style)", "content: attr(styles)", "content: \"attr\"(style)", "x: data-attr(style)", "url(style)", ""] {
            assert!(!names_attr_style(no), "{no}");
        }
    }

    #[test]
    fn an_image_set_requests_its_first_candidate_only() {
        assert_eq!(image_urls("image-set(url(a.png) 1x, url(b.png) 2x)"), vec!["a.png"]);
        assert_eq!(image_urls("url(m1.png), url(m2.png)"), vec!["m1.png", "m2.png"]);
        assert_eq!(image_urls("linear-gradient(red, blue)"), Vec::<String>::new());
    }
}
