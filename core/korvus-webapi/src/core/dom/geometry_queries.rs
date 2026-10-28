//! `core/dom/geometry_queries.rs` -- the bridge from an element to where it is on the page.
//!
//! `document.body.getBoundingClientRect().width === 0` is the detect this whole phase exists to
//! close: every layout-dependent member returned zero, because there was no layout. The
//! Element/HTMLElement rows here -- `getBoundingClientRect`, `getClientRects`, `clientTop/Left/Width/
//! Height`, `offsetWidth/Height/Left/Top` -- ask `korvus-page` instead. They used to be JS bridge
//! helpers (dom/03-element-glue.js) over five runtime ops (core/korvus-runtime/src/layout.rs); the
//! ops that remain (`elementFromPoint`, `elementsFromPoint`) moved here with them, and the runtime
//! keeps none.
//!
//! # The cache lives here, not in the pipeline
//!
//! A laid-out document is not `Sync` -- taffy's `Style` carries a raw pointer for `calc()` values --
//! so it cannot sit in a service on the kernel `Context`. `OpState` is the right home for it anyway:
//! one V8 isolate owns one thread and one document, which is exactly the scope a layout cache is
//! valid for.
//!
//! A page measures elements in a loop. Laying the document out again for each one turns a linear
//! read into a quadratic one, so the result is kept and re-used -- keyed on a fingerprint of
//! everything layout depends on, so a page that measures, mutates, and measures again does *not*
//! get the first answer. The runtime resets the cache at every navigation (`load_page_inner`): the
//! key is the DOM's own revision counter, and a freshly parsed document starts at zero.
//!
//! # Client coordinates
//!
//! The cache is keyed on what LAYOUT depends on, and a scroll is not among it: scrolling moves no box.
//! What it moves is where the boxes are seen from, so every refresh hands the cached layout the
//! current scroll offsets ([`scroll_snapshot`]) and the members ask it client-coordinate questions.
//! Chrome 150.0.7871.46, `scrollTo(0, 900)` over a box at `top: 1000px`: `offsetTop` 1000,
//! `getBoundingClientRect().top` 100 -- where this engine answered 1000 for both.
//!
//! # Frames
//!
//! A nested browsing context's document lives in the same arena as the page's, under its own root,
//! and nothing laid it out: every element in it answered `[0, 0, 0, 0]`. [`FrameLayouts`] holds one
//! laid-out document per frame root, laid out into the frame element's content box -- LAZILY, when a
//! geometry question is asked about a node in that document and not before, so a page that never
//! measures inside a frame (or has none) lays out nothing more than it did. The count is
//! [`layout_counts`].

use deno_core::{v8, OpState};
use page::style::media::MediaContext;
use page::{LaidOut, PageHandle};

use crate::binding::host_dom::HostDom;
use crate::binding::protos::{new_global, unwrap_id};
use crate::core::dom::collections::static_list_reuse;

/// The last laid-out document and the fingerprint it was laid out from.
#[derive(Default)]
pub struct LayoutCache(pub Option<(u64, LaidOut)>);

/// Everything the page's geometry fingerprint reads, as one value ([`page::Page::geometry_counters`]): equal to the
/// last refresh's, the fingerprint is the last one.
#[derive(Clone, Copy, PartialEq)]
struct RefreshKey {
    revision: u64,
    instance: u64,
    mode: korvus_dom::DocumentMode,
    media: MediaContext,
    images: u64,
    /// The font bodies kept ([`push_web_fonts`]).
    fonts: u64,
    counters: (u64, u64, u64, u64),
}

/// The last page refresh's [`RefreshKey`], taken after its fingerprint (whose sheet sync can move the counters), and
/// that fingerprint. Never reset: a navigation's reset of [`LayoutCache`] or a new arena breaks the match on its own.
#[derive(Default)]
struct RefreshMemo(Option<(RefreshKey, u64)>);

/// Bumped whenever a page or frame layout cache takes a NEW layout, and whenever the page's is found empty (a
/// navigation or a realm reset it): a consumer that derived something from the layouts -- intersections, observed
/// sizes -- and keeps the epoch it saw need not derive it again while the epoch stands. Process-wide and never
/// reset, so no two documents share a value. It does NOT move on scroll: a scroll changes no layout (see
/// [`scroll_snapshot`]), so a consumer of client coordinates keys on the scroll as well.
static LAYOUT_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// See [`LAYOUT_EPOCH`]. For the observers' refresh (IntersectionObserver, ResizeObserver, lazy loading), which do
/// not read it yet.
#[allow(dead_code)]
pub fn layout_epoch() -> u64 {
    LAYOUT_EPOCH.load(std::sync::atomic::Ordering::Relaxed)
}

fn new_layout_epoch() {
    LAYOUT_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

fn refresh_key(state: &OpState, page: &page::Page, media: &MediaContext) -> RefreshKey {
    let dom = &state.borrow::<HostDom>().0;
    RefreshKey {
        revision: dom.revision(),
        instance: dom.instance(),
        mode: dom.document_mode(dom.root()),
        media: *media,
        images: state.try_borrow::<NaturalImages>().map_or(0, |n| n.generation),
        fonts: font_bodies(state).map_or(0, |b| b.generation()),
        counters: page.geometry_counters(),
    }
}

/// The decoded natural size of every `<img>` of this document whose image is available, which of them
/// are broken (A/B C8: Blink lays a broken image out as its fallback content), and a generation that
/// moves whenever one arrives or goes. Written by the element loader when an image load
/// completes (`core/html/subresource.rs`), handed to the layout before each one
/// (`Page::set_natural_images`), and reset with the document -- an element id means nothing in the next.
#[derive(Default)]
pub struct NaturalImages {
    pub map: std::collections::HashMap<u32, page::ImageState>,
    pub generation: u64,
    /// The URL each `<img>`'s current request is for, as the loader made it (`subresource::image_update`): its
    /// selected `srcset` candidate (or `<picture>` source), else its `src`. What its size, pixels and `currentSrc`
    /// are read under -- read under its `src`, every `srcset` image was a 0x0 image never painted -- with the
    /// candidate's pixel density, which its natural size is divided by.
    pub sources: std::collections::HashMap<u32, (String, f32)>,
}

impl NaturalImages {
    /// Record `img`'s state (or its absence), moving the generation only when the answer changed.
    pub(crate) fn set(state: &mut OpState, img: u32, image: Option<page::ImageState>) {
        if state.try_borrow::<NaturalImages>().is_none() {
            state.put(NaturalImages::default());
        }
        let n = state.borrow_mut::<NaturalImages>();
        let changed = match image {
            Some(s) => n.map.insert(img, s) != Some(s),
            None => n.map.remove(&img).is_some(),
        };
        if changed {
            n.generation = n.generation.wrapping_add(1);
        }
    }

    /// Record (or forget) the URL `img`'s current request is for ([`NaturalImages::sources`]).
    pub(crate) fn set_source(state: &mut OpState, img: u32, url: Option<(String, f32)>) {
        if state.try_borrow::<NaturalImages>().is_none() {
            state.put(NaturalImages::default());
        }
        let n = state.borrow_mut::<NaturalImages>();
        match url {
            Some(u) => {
                n.sources.insert(img, u);
            }
            None => {
                n.sources.remove(&img);
            }
        }
    }

    /// A new request for `img` is in flight or deferred: its image is "potentially available" again,
    /// so a broken one is no longer laid out as its fallback content (Blink's `ResetLayoutDisposition`
    /// -> `EnsurePrimaryContent`). An available image keeps its size until the new one settles.
    pub(crate) fn unbreak(state: &mut OpState, img: u32) {
        let broken = state.try_borrow::<NaturalImages>().is_some_and(|n| n.map.get(&img) == Some(&page::ImageState::Broken));
        if broken {
            NaturalImages::set(state, img, None);
        }
    }
}

/// Hand this document's image states to the page pipeline before a fingerprint is taken.
fn push_natural_images(state: &OpState, page: &page::Page) {
    if let Some(n) = state.try_borrow::<NaturalImages>() {
        page.set_natural_images(&n.map, n.generation);
    }
}

fn font_bodies(state: &OpState) -> Option<std::sync::Arc<crate::core::fonts::font_probe::FontProbes>> {
    state.try_borrow::<spec::kernel::Context>()?.get::<crate::core::fonts::font_probe::FontProbes>()
}

/// Hand this document's web fonts to the page pipeline before a fingerprint is taken: the `@font-face` faces whose
/// fonts have arrived, each with the metrics the render backend reads off its file, remade when a font body arrives
/// or the sheets change (the page keeps the key it was made from, so an unchanged one costs a hash).
fn push_web_fonts(state: &OpState, page: &page::Page) {
    let Some(bodies) = font_bodies(state) else { return };
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (bodies.generation(), page.geometry_counters().0).hash(&mut h);
        h.finish() | 1
    };
    if page.web_fonts_key() == key {
        return;
    }
    let table = (|| {
        let render = state.try_borrow::<spec::kernel::Context>()?.get::<spec::RenderHandle>()?;
        let faces = crate::core::css::sheet_loads::WebFaces::of(state, &media_context(state))?;
        let mut table = page::font::WebFonts::default();
        for (family, style, weight, ranges, url) in faces.each() {
            let Some(body) = bodies.body(url) else { continue };
            let Some(name) = render.0.web_font(url, &body) else { continue };
            let Some(metrics) = page::font::web_font_metrics(&name, || render.0.web_font_metrics(&name)) else { continue };
            // a variable font measures at each weight and size its text is set in, under the instance's name
            let instance = page::font::web_font_weight_axis(&name).map(|_| {
                let (name, render) = (name.clone(), render.0.clone());
                // per size only where the font has an optical-size axis: another is the same at every size
                let sized = page::font::web_font_has_opsz(&name);
                page::font::WeightInstances(std::sync::Arc::new(move |w: u16, size: f32| {
                    let at = if sized { page::font::instance_name(&name, w, size) } else { format!("{name}@{w}") };
                    page::font::web_font_metrics(&at, || render.web_font_metrics(&at))
                }))
            });
            table.add(family, page::font::WebFace { weight, style: style.to_string(), ranges: ranges.to_vec(), metrics, instance });
        }
        Some(std::sync::Arc::new(table))
    })();
    page.set_web_fonts(table, key);
}

/// The laid-out document of every frame something has measured in, by the frame document's arena
/// root, with the fingerprint each was laid out from; and how many documents have been laid out.
#[derive(Default)]
pub struct FrameLayouts {
    frames: std::collections::HashMap<u32, (u64, LaidOut)>,
    /// Bumped whenever the page's own cache is found empty -- a navigation reset it -- so no frame
    /// layout from the previous document can be taken for one of this document's.
    epoch: u64,
    /// The page's document laid out, and a frame's.
    page_layouts: u64,
    frame_layouts: u64,
}

/// `(page layouts, frame layouts)` this realm has run -- for the test that a page with no frame, or
/// a frame nobody measures in, lays out nothing extra.
pub fn layout_counts(state: &OpState) -> (u64, u64) {
    state.try_borrow::<FrameLayouts>().map_or((0, 0), |f| (f.page_layouts, f.frame_layouts))
}

/// How many documents `korvus-page` has actually laid out.
///
/// [`layout_counts`] counts the refreshes THIS CACHE ran, which is the right number for P13a's
/// question and the wrong one for C1's: `Page::computed_style` lays the document out behind the
/// cache when a `@container` rule's basis has moved, and that layout is invisible here. This is
/// `Page::layouts_run`, the count kept where the layout happens -- so "a second style read of an
/// unchanged tree lays nothing out" is a claim that can be asserted rather than timed. Rust only, a
/// count and not a clock, exactly like its neighbour.
pub fn document_layouts(state: &OpState) -> u64 {
    state
        .try_borrow::<spec::kernel::Context>()
        .and_then(|c| c.get::<PageHandle>())
        .map_or(0, |h| h.0.layouts_run())
}

/// The scroll offsets a layout of the document whose element is `document_element` is seen through.
///
/// The PAGE's viewport scroll is `window.scrollX`/`scrollY`'s one cell; a frame's is its own document
/// element's `scrollLeft`/`scrollTop`, since a frame window's `scrollTo` scrolls that element. Every
/// other element's offset is its own.
fn scroll_snapshot(state: &OpState, document_element: Option<u32>, page: bool) -> page::layout::geometry::ScrollSnapshot {
    let elements: std::collections::HashMap<u32, (f32, f32)> = crate::core::dom::element::scrolled_elements(state)
        .into_iter()
        .map(|(id, (x, y))| (id, (x as f32, y as f32)))
        .collect();
    let viewport = if page {
        let (x, y) = crate::core::html::window_geometry::scroll_position(state);
        (x as f32, y as f32)
    } else {
        document_element.and_then(|de| elements.get(&de).copied()).unwrap_or((0.0, 0.0))
    };
    page::layout::geometry::ScrollSnapshot { viewport, elements }
}

/// What the media queries and layout are asked about, taken from the persona service.
///
/// The viewport is the WINDOW's inner size, not the screen's. Using the screen made
/// `@media (height: 982px)` match on a desktop whose work area is 884 tall, and disagree with the
/// `window.innerHeight` a script reads -- the same value reported two different ways.
///
/// THE FALLBACK CHAIN IS NO LONGER MIRRORED HERE. This function used to carry its own
/// copy of it, with a comment saying so: "lives once in the engine's window_geometry op; mirrored
/// here because the cascade cannot call an op". The op is gone -- the chain is
/// [`crate::core::html::window_geometry::window_geometry`], plain Rust in this crate, which the
/// `Window` table's `innerWidth` row and this function now both call. Two copies of a fallback chain
/// can disagree; one cannot, and `@media (height: …)` versus `window.innerHeight` is exactly the
/// pair where a disagreement is a coherence tell rather than a missing feature.
///
/// A browser window is never 0x0, so a persona with no screen at all gets the same 1024x768 the
/// style layer defaults to rather than a page with no geometry.
pub fn media_context(state: &OpState) -> MediaContext {
    let default = MediaContext::default();
    let Some(persona) = crate::binding::protos::persona_handle(&state) else {
        return default;
    };
    let screen = spec::devtools::screen(persona.0.screen());
    // `window_geometry_of` over the screen already in hand, rather than `window_geometry(state)`,
    // which looked the persona up and copied its screen a second time. This runs once per
    // `getComputedStyle` call; the numbers are the same function of the same profile.
    let [win_w, win_h, ..] = crate::core::html::window_geometry::window_geometry_of(&screen);
    // A remote frame's document lays out in its `<iframe>`'s content box.
    let (win_w, win_h) = crate::core::html::remote_frame::embedded_viewport(state).unwrap_or((win_w, win_h));
    let width = (win_w > 0.0).then_some(win_w as f32).unwrap_or(default.viewport_width);
    let height = (win_h > 0.0).then_some(win_h as f32).unwrap_or(default.viewport_height);
    // `device_*` stays the SCREEN -- that is what `@media (device-height)` asks about.
    let dev_w = (screen.width > 0).then_some(screen.width as f32).unwrap_or(default.device_width);
    let dev_h = (screen.height > 0).then_some(screen.height as f32).unwrap_or(default.device_height);
    MediaContext {
        viewport_width: width,
        viewport_height: height,
        device_width: dev_w,
        device_height: dev_h,
        dppx: if screen.device_pixel_ratio > 0.0 { screen.device_pixel_ratio as f32 } else { 1.0 },
        color_bits: if screen.color_depth > 0 { screen.color_depth / 3 } else { 8 },
        // The worn device's scrollbar and form-control theme. A persona that records
        // none lays out with none -- never with a thickness this crate picked.
        theme: persona.0.theme().unwrap_or_default(),
        // a remote frame's `<iframe scrolling=no>`
        scrollbars_off: crate::core::html::remote_frame::embedded_scrollbars_off(state),
        // What a DevTools client emulates (`Emulation.setEmulatedMedia`, touch emulation): the media type and the
        // user preferences it named, over the device's own.
        print: spec::devtools::emulated_media().is_some_and(|m| m.media == "print"),
        prefers_dark: spec::devtools::media_feature("prefers-color-scheme").is_some_and(|v| v == "dark"),
        prefers_reduced_motion: spec::devtools::media_feature("prefers-reduced-motion").is_some_and(|v| v == "reduce"),
        coarse_pointer: spec::devtools::touch_points().is_some_and(|n| n > 0),
        hover: !spec::devtools::touch_points().is_some_and(|n| n > 0),
        ..default
    }
}

/// Bring the layout cache up to date, and say whether there is one to use.
///
/// [`measure`] does this and then answers a geometry question in the same call, which is right for
/// the geometry members and wrong for `getComputedStyle`: that one needs the PAGE and the DOM as well
/// as the boxes, and cannot get them through a closure that only receives `&LaidOut`. So the refresh
/// is available on its own (`css::cssom::css_computed_all` calls it; that was the runtime's
/// `op_css_computed_all` until the op was deleted), and the caller reads
/// [`LayoutCache`] itself.
pub fn refresh_layout_cache(state: &mut OpState) -> bool {
    let Some(handle) = state.try_borrow::<spec::kernel::Context>().and_then(|ctx| ctx.get::<PageHandle>()) else { return false };
    if state.try_borrow::<HostDom>().is_none() {
        return false;
    }
    let media = media_context(state);
    let document_element = {
        let dom = &state.borrow::<HostDom>().0;
        crate::core::dom::document::doc_element_child(dom, dom.root())
    };
    let cached = state.try_borrow::<LayoutCache>().and_then(|c| c.0.as_ref().map(|(c, _)| *c));
    // A CLEAN read: nothing the fingerprint reads has moved since the last refresh took it, and the cache still holds
    // the layout of that fingerprint -- so it is current, and neither the fingerprint (a sheet sync, a lock, a style
    // reading check, a hash of fifteen numbers) nor the images' hand-over is owed.
    let key = refresh_key(state, &handle.0, &media);
    let memo = state.try_borrow::<RefreshMemo>().and_then(|m| m.0);
    let clean = matches!((cached, memo), (Some(fp), Some((k, f))) if f == fp && k == key);
    if clean && !page::Page::verifying() {
        let scroll = scroll_snapshot(state, document_element, true);
        if let Some(LayoutCache(Some((_, out)))) = state.try_borrow_mut::<LayoutCache>() {
            out.set_scroll(scroll);
        }
        return true;
    }
    push_natural_images(state, &handle.0);
    push_web_fonts(state, &handle.0);
    let (fingerprint, style_key) = {
        let dom = &state.borrow::<HostDom>().0;
        let fingerprint = handle.0.page_geometry_fingerprint(dom, &media);
        // and what `transform`, `opacity` and the clips below read on top of it (D77)
        (fingerprint, handle.0.effects_fingerprint(dom, fingerprint))
    };
    if clean && cached != Some(fingerprint) {
        page::Page::verify_mismatch(&format!("geometry refresh memo: the inputs it keys on held, the fingerprint moved ({cached:?} -> {fingerprint})"));
    }
    // What the fingerprint was taken from: after its own sheet sync, before a relayout -- which can move the
    // container-size generation, and then the next read's fingerprint with it.
    let key = refresh_key(state, &handle.0, &media);
    if cached != Some(fingerprint) {
        // The last layout, when there is one, is HANDED to the relayout, which moves what it carries out of it
        // rather than copying it, and comes back -- as it was, when the document would be laid out just as it was,
        // or as the new layout. Whatever comes back is what the cache holds: no script runs in between.
        let previous = state.try_borrow_mut::<LayoutCache>().and_then(|c| c.0.take()).map(|(_, out)| out);
        let (out, new) = {
            let dom = &state.borrow::<HostDom>().0;
            // A container settle that a style read just ran laid this very document out (`Page::take_settled_layout`).
            match (handle.0.take_settled_layout(dom, &media, fingerprint), previous) {
                (Some(settled), _) => (settled, true),
                (None, Some(previous)) => {
                    let relaid = handle.0.relayout_document_owned(dom, dom.root(), &media, previous);
                    let new = relaid.is_new();
                    (relaid.into_inner(), new)
                }
                (None, None) => (handle.0.layout(dom, &media), true),
            }
        };
        if new || cached.is_none() {
            new_layout_epoch();
        }
        if state.try_borrow::<FrameLayouts>().is_none() {
            state.put(FrameLayouts::default());
        }
        let frames = state.borrow_mut::<FrameLayouts>();
        frames.page_layouts += 1;
        if cached.is_none() {
            frames.epoch += 1;
            frames.frames.clear();
        }
        state.put(LayoutCache(Some((fingerprint, out))));
    }
    state.put(RefreshMemo(Some((key, fingerprint))));
    let scroll = scroll_snapshot(state, document_element, true);
    if let Some(LayoutCache(Some((_, out)))) = state.try_borrow_mut::<LayoutCache>() {
        out.set_scroll(scroll);
    }
    // What each box's style does beyond layout -- `transform`, `opacity` (D77) -- read again when the style changed:
    // neither relays anything out (`Dom::inert_style_writes`), so the geometry fingerprint above does not see them, and
    // the layout fingerprint, which counts every DOM write, every sheet and every animation sample, does. The paint
    // walk, hit testing and the client rects all read what is kept here.
    let stale = state.try_borrow::<LayoutCache>().and_then(|c| c.0.as_ref()).is_some_and(|(_, out)| out.effects_key() != Some(style_key));
    if stale {
        let effects = match (state.try_borrow::<HostDom>(), state.try_borrow::<LayoutCache>().and_then(|c| c.0.as_ref())) {
            (Some(dom), Some((_, out))) => Some(handle.0.box_effects(&dom.0, &media, out)),
            _ => None,
        };
        if let (Some(effects), Some(LayoutCache(Some((_, out))))) = (effects, state.try_borrow_mut::<LayoutCache>()) {
            out.set_effects(style_key, effects);
        }
    }
    true
}

/// Bring the layout of the FRAME document rooted at `root` up to date; `false` when no frame shows
/// that document, or the frame element has no box to lay it out into.
///
/// The frame's viewport is its element's CONTENT box, which is measured, not assumed (Chrome 150): a
/// `300x200` iframe with `padding: 5px` and the default 2px border answers `innerWidth` 300 and lays
/// its document out 300 wide. The element's own document is brought up to date first -- which is the
/// page's, or for a frame inside a frame, another frame's.
fn refresh_frame_layout(state: &mut OpState, root: u32, depth: u32) -> bool {
    let Some((width, height)) = frame_box(state, root, depth) else { return false };
    let Some(handle) = state.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<PageHandle>()) else { return false };
    let mut media = media_context(state);
    media.viewport_width = width;
    media.viewport_height = height;
    let Some(dom) = state.try_borrow::<HostDom>() else { return false };
    // an `<iframe scrolling=no>` lays its document out with no scrollbars
    media.scrollbars_off = crate::core::html::browsing_context::frame_element_showing(state, root).is_some_and(|el| crate::core::html::remote_frame::scrolling_off(&dom.0, el));
    let epoch = state.try_borrow::<FrameLayouts>().map_or(0, |f| f.epoch);
    let fingerprint = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        push_natural_images(state, &handle.0);
        push_web_fonts(state, &handle.0);
        // Its own document's changes, not the arena's: keyed on the arena's, a page write or another
        // frame's relaid every frame out at its next style or geometry read.
        handle.0.document_geometry_fingerprint(&dom.0, &media, root).hash(&mut h);
        media.scrollbars_off.hash(&mut h);
        root.hash(&mut h);
        epoch.hash(&mut h);
        h.finish()
    };
    let document_element = crate::core::dom::document::doc_element_child(&dom.0, root);
    let cached = state.try_borrow::<FrameLayouts>().and_then(|f| f.frames.get(&root).map(|(c, _)| *c));
    if cached != Some(fingerprint) {
        // As for the page: the frame's last layout is handed over, and what comes back -- it, when it would be laid
        // out just as it was, or the new one -- is what the frame keeps.
        let previous = state.try_borrow_mut::<FrameLayouts>().and_then(|f| f.frames.remove(&root)).map(|(_, out)| out);
        let (out, new) = {
            let dom = &state.borrow::<HostDom>().0;
            match previous {
                Some(previous) => {
                    let relaid = handle.0.relayout_document_owned(dom, root, &media, previous);
                    let new = relaid.is_new();
                    (relaid.into_inner(), new)
                }
                None => (handle.0.layout_document(dom, root, &media), true),
            }
        };
        if new {
            new_layout_epoch();
        }
        if state.try_borrow::<FrameLayouts>().is_none() {
            state.put(FrameLayouts::default());
        }
        let frames = state.borrow_mut::<FrameLayouts>();
        frames.frame_layouts += 1;
        frames.frames.insert(root, (fingerprint, out));
    }
    let scroll = scroll_snapshot(state, document_element, false);
    if let Some((_, out)) = state.try_borrow_mut::<FrameLayouts>().and_then(|f| f.frames.get_mut(&root)) {
        out.set_scroll(scroll);
    }
    true
}

/// The content box of the frame element showing the frame document `root` -- that document's viewport --
/// measured in the element's own document (the page's, or another frame's). `None` when no frame shows
/// the document, or its element has no box.
fn frame_box(state: &mut OpState, root: u32, depth: u32) -> Option<(f32, f32)> {
    if depth > 16 {
        return None;
    }
    // a popup's document lays out in its own window, the size of the page's (a DevTools client sees it as a page)
    if crate::core::html::browsing_context::popup_document(state, root) {
        let m = media_context(state);
        return Some((m.viewport_width, m.viewport_height));
    }
    let el = crate::core::html::browsing_context::frame_element_showing(state, root)?;
    let el_root = state.try_borrow::<HostDom>()?.0.shadow_including_root(el);
    let page_root = state.try_borrow::<HostDom>().map(|d| d.0.root());
    let size = if Some(el_root) == page_root {
        if !refresh_layout_cache(state) {
            return None;
        }
        match state.try_borrow::<LayoutCache>() {
            Some(LayoutCache(Some((_, out)))) => out.content_box(el),
            _ => None,
        }
    } else {
        if !refresh_frame_layout(state, el_root, depth + 1) {
            return None;
        }
        state.try_borrow::<FrameLayouts>().and_then(|f| f.frames.get(&el_root)).and_then(|(_, out)| out.content_box(el))
    };
    size.map(|s| (s.width, s.height))
}

/// What a FRAME document's style is resolved against: the page's media context with the
/// frame's own viewport -- its element's content box, 0x0 when the element has no box (Chrome 150 still
/// answers a `display:none` frame's computed style). `None` when `root` is no frame's document.
pub(crate) fn frame_media(state: &mut OpState, root: u32) -> Option<MediaContext> {
    let el = crate::core::html::browsing_context::frame_element_showing(state, root)?;
    let (width, height) = frame_box(state, root, 0).unwrap_or((0.0, 0.0));
    let mut media = media_context(state);
    media.viewport_width = width;
    media.viewport_height = height;
    media.scrollbars_off = state.try_borrow::<HostDom>().is_some_and(|h| crate::core::html::remote_frame::scrolling_off(&h.0, el));
    Some(media)
}

/// Bring the layout of the FRAME document rooted at `root` up to date (F27: a computed value resolved from
/// layout); `false` when no frame shows it or its element has no box. [`frame_laid_out`] then reads it.
pub(crate) fn refresh_frame(state: &mut OpState, root: u32) -> bool {
    refresh_frame_layout(state, root, 0)
}

/// Whether the FRAME document rooted at `root` is being rendered: a frame shows it and its element has a
/// box (F27: `innerText` there walks the rendering; a `display:none` frame's is its text content, as in
/// Chrome 150).
pub(crate) fn frame_document_rendered(state: &mut OpState, root: u32) -> bool {
    frame_box(state, root, 0).is_some()
}

/// The laid-out FRAME document rooted at `root`, as [`refresh_frame`] left it.
pub(crate) fn frame_laid_out(state: &OpState, root: u32) -> Option<&LaidOut> {
    state.try_borrow::<FrameLayouts>()?.frames.get(&root).map(|(_, out)| out)
}

/// The document a node is laid out in: `Some(None)` for the page's, `Some(Some(root))` for a frame
/// document's, `None` for a node in no laid-out document (detached, or a `DOMParser` document's).
fn document_of(state: &OpState, node: u32) -> Option<Option<u32>> {
    let dom = &state.try_borrow::<HostDom>()?.0;
    let root = dom.shadow_including_root(node);
    if root == dom.root() {
        return Some(None);
    }
    (dom.node_type(root) == 9).then_some(Some(root))
}

/// [`measure`] for a question about ONE node: laid out in whichever document the node is in.
fn measure_node<T>(state: &mut OpState, node: u32, absent: T, f: impl FnOnce(&LaidOut) -> T) -> T {
    match document_of(state, node) {
        Some(None) | None => measure(state, absent, f),
        Some(Some(root)) => {
            if !refresh_frame_layout(state, root, 0) {
                return absent;
            }
            match state.try_borrow::<FrameLayouts>().and_then(|fl| fl.frames.get(&root)) {
                Some((_, out)) => f(out),
                None => absent,
            }
        }
    }
}

/// [`measure_node`] with the DOM in hand as well, borrowed after the refresh: a question that walks the node's
/// ancestors and asks the layout about each (`offsetParent`, whether any ancestor transforms) asks it in one
/// refresh, with no list of them built first.
fn measure_node_with_dom<T>(state: &mut OpState, node: u32, absent: T, f: impl FnOnce(&LaidOut, &korvus_dom::Dom) -> T) -> T {
    match document_of(state, node) {
        Some(None) | None => {
            if !refresh_layout_cache(state) {
                return absent;
            }
            match (state.try_borrow::<LayoutCache>(), state.try_borrow::<HostDom>()) {
                (Some(LayoutCache(Some((_, out)))), Some(dom)) => f(out, &dom.0),
                _ => absent,
            }
        }
        Some(Some(root)) => {
            if !refresh_frame_layout(state, root, 0) {
                return absent;
            }
            match (state.try_borrow::<FrameLayouts>().and_then(|fl| fl.frames.get(&root)), state.try_borrow::<HostDom>()) {
                (Some((_, out)), Some(dom)) => f(out, &dom.0),
                _ => absent,
            }
        }
    }
}

/// Lay the document out if anything it depends on has moved, then answer `f` about it.
///
/// `absent` is passed rather than taken from `Default`, because the two are not always the same
/// value and the difference is a bug that only appears when the pipeline is missing.
/// `elementFromPoint` reports "nothing there" as `-1`, and `i32::default()` is `0` -- a perfectly
/// good node id -- so a build without the page plugin answered `<html>` for every point on the page
/// instead of `null`. Making the caller say what absent means is what stops that from recurring.
fn measure<T>(state: &mut OpState, absent: T, f: impl FnOnce(&LaidOut) -> T) -> T {
    if !refresh_layout_cache(state) {
        return absent;
    }
    match state.try_borrow::<LayoutCache>() {
        Some(LayoutCache(Some((_, out)))) => f(out),
        _ => absent,
    }
}

// `op_layout_element_from_point` and `op_layout_elements_from_point` ARE GONE. They were
// the JS side's way into the hit test; the `Document` rows below ([`cb_doc_element_from_point`],
// [`cb_doc_elements_from_point`]) call these two plain Rust fns, which is what the ops' own docs
// already said was the point of splitting them out. Nothing called the ops.

/// `document.elementFromPoint`'s answer: the node id, or `-1` for a point on no box at all.
///
/// A signed integer rather than an `Option` because the caller turns it into an element or `null`,
/// and `-1` is used because `0` is a perfectly good node id (see [`measure`]).
///
/// `root` is the receiving document's arena root: the page's, or a frame's (Chrome
/// answers `#in` for a point on it inside `frame.contentDocument`).
pub(crate) fn element_from_point(state: &mut OpState, root: u32, x: f32, y: f32) -> i32 {
    // `-1`, not `0`: zero is a node id.
    measure_node(state, root, -1, |out| out.element_from_point(x, y).map_or(-1, |n| n as i32))
}

/// `document.elementsFromPoint` -- the whole stack at a point, topmost first. See
/// [`element_from_point`].
fn elements_from_point(state: &mut OpState, root: u32, x: f32, y: f32) -> Vec<u32> {
    measure_node(state, root, Vec::new(), |out| out.elements_from_point(x, y))
}

/// `op_node_id_of` WAS HERE. It handed an element's arena id to the one JS caller that
/// needed it -- `host.js`'s `getComputedStyle` -- because `nodeId` is hidden from the element's
/// public surface on purpose (a real browser has no such member) and the JS could not reach
/// `unwrap_id`. `getComputedStyle` is `core/css/cssom.rs::cb_get_computed_style` now and calls
/// `binding::protos::unwrap_id` directly, so the private accessor has no reader left.

fn node_of(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> Option<u32> {
    unwrap_id(scope, args.this().into())
}

/// [`measure`] for a member body: the receiver's node, the op state borrowed for the duration.
///
/// A geometry read is a STYLE flush first (a bot-detection service D7): Blink's `offsetHeight`,
/// `getBoundingClientRect` and the rest run `Document::UpdateStyleAndLayoutForNode`, whose style recalc is where a
/// CSS transition starts against the before-change style it computed last time -- so an element styled once by an
/// `offsetHeight` read and then given `opacity: 0.3` under `transition: opacity 5s` transitions (Chrome 150: opacity
/// `1`, one `CSSTransition`). Only `getComputedStyle` ran this engine's equivalent, the animation tick, so here the
/// value jumped to `0.3` with no animation.
fn measured<T>(scope: &mut v8::PinScope, node: Option<u32>, absent: T, f: impl FnOnce(&LaidOut, u32) -> T) -> T {
    let Some(n) = node else { return absent };
    crate::core::animation::css::flush_style_for(scope, n);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    measure_node(&mut st, n, absent, |out| f(out, n))
}

/// [`measured`] with the DOM in hand as well ([`measure_node_with_dom`]).
fn measured_with_dom<T>(scope: &mut v8::PinScope, node: Option<u32>, absent: T, f: impl FnOnce(&LaidOut, &korvus_dom::Dom, u32) -> T) -> T {
    let Some(n) = node else { return absent };
    crate::core::animation::css::flush_style_for(scope, n);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    measure_node_with_dom(&mut st, n, absent, |out, dom| f(out, dom, n))
}

/// Does `node` or any element above it in the flat tree (a shadow root's host above its children) carry a transform
/// that applies -- the chain [`transformed_as_layout`] walks? When none does, that walk hands the rects back as they
/// are, so a caller that already knows can skip it: it laid the document out again (a fingerprint check) and measured
/// every ancestor's rect, O(depth^2), on every `getBoundingClientRect`.
fn any_transform(out: &LaidOut, dom: &korvus_dom::Dom, node: u32) -> bool {
    let mut cur = Some(node);
    while let Some(n) = cur {
        if dom.node_type(n) == 1 && out.transforms(n) {
            return true;
        }
        cur = dom.parent(n).and_then(|p| if dom.is_shadow_root(p) { dom.shadow_host_of(p) } else { Some(p) });
    }
    false
}

/// Element `node`'s border box in client coordinates through every transform above it -- `getBoundingClientRect`'s
/// answer, `[x, y, w, h]` -- and its padding box less scrollbars moved as far, or `None` when it has no box. What a
/// lazy-load observation measures: a carousel slide brought into view by a `transform` on its track is in view
/// (a production site's Swiper banners), where the untransformed layout put it a thousand pixels to the right.
pub(crate) fn transformed_boxes(scope: &mut v8::PinScope, node: u32) -> Option<([f64; 4], Option<[f64; 4]>)> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (r, pad, transforms) = {
        let mut st = op_state.borrow_mut();
        measure_node_with_dom(&mut st, node, None, |out, dom| {
            if out.client_rects(node).is_empty() {
                return None;
            }
            let r = out.viewport_bounding_rect(node);
            let pad = out.viewport_padding_box(node).map(|p| [p.x as f64, p.y as f64, p.width as f64, p.height as f64]);
            Some(([r.x as f64, r.y as f64, r.width as f64, r.height as f64], pad, any_transform(out, dom, node)))
        })
    }?;
    if !transforms {
        return Some((r, pad));
    }
    let t = transformed(scope, node, vec![r]).pop().unwrap_or(r);
    let (dx, dy) = (t[0] - r[0], t[1] - r[1]);
    Some((t, pad.map(|p| [p[0] + dx, p[1] + dy, p[2], p[3]])))
}

fn rect_args<'s>(scope: &mut v8::PinScope<'s, '_>, r: [f64; 4]) -> Vec<v8::Local<'s, v8::Value>> {
    r.iter().map(|&n| v8::Number::new(scope, n).into()).collect()
}

/// `getBoundingClientRect()`: a fresh `DOMRect`. An element with no box gives four zeros rather than
/// nothing, which is what a browser gives for `display: none` -- and, until inline flow lands, for an
/// inline element too.
pub(crate) fn cb_el_bounding_client_rect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let node = node_of(scope, &args);
    let (r, transforms) = measured_with_dom(scope, node, ([0.0f64; 4], false), |out, dom, n| {
        let r = out.viewport_bounding_rect(n);
        ([r.x as f64, r.y as f64, r.width as f64, r.height as f64], any_transform(out, dom, n))
    });
    let r = match node {
        Some(n) if transforms => transformed(scope, n, vec![r]).pop().unwrap_or(r),
        _ => r,
    };
    let vals = rect_args(scope, r);
    if let Some(o) = new_global(scope, "DOMRect", &vals) {
        rv.set(o.into());
    }
}

/// `rects` -- element `node`'s boxes in viewport coordinates, as laid out -- through the CSS transforms
/// of `node` and of every ancestor in the flat tree (a shadow root's host above its children), each
/// about its own `transform-origin`: each rect becomes the bounding box of its transformed quad
/// (Blink's `LocalToAbsoluteQuad` and `BoundingBox`). A 3D matrix is projected (divided by w) and
/// flattened at each level, as `transform-style: flat` does; the quad is in float, as Blink's `QuadF`.
///
/// Measured on Chrome 150 (fixtures/transforms): `skewY(23.1753218deg)` on a 100x40 box reads 82.809px
/// tall at y -13.4045, `perspective(12890px) translateZ(101.5px)` grows it to 100.79 x 40.32 -- every
/// one of them read the untransformed box here. A transform applies to the box itself and everything
/// it contains, so an ancestor's moves the descendants it contains as well.
fn transformed(scope: &mut v8::PinScope, node: u32, rects: Vec<[f64; 4]>) -> Vec<[f64; 4]> {
    match transformed_as_layout(scope, node, &rects) {
        Some(out) => out,
        None => transformed_textbook(scope, node, rects),
    }
}

/// [`transformed`] as Blink's layout maps a box's quad to the page (`LayoutObject::LocalToAncestorQuad`, a
/// `TransformState` in the apply direction, flattened at each transform): the box's LOCAL corners, in float, go up
/// its chain of containers (`LayoutObject::Container`); a container with no transform only adds its offset -- in
/// layout units, so exactly -- and a transforming box maps the quad through its transform from its container
/// (`GetTransformFromContainer`: its own `gfx::Transform`, then `PostTranslate` by its offset in that container),
/// after the offsets gathered below it are added in float (`ApplyAccumulatedOffset`). Then the float bounding box,
/// less the viewport's scroll. Chrome 150 reads a bot challenge's ClientRects probe exactly this way; mapped
/// textbook-style -- origin-relative viewport corners through one double matrix -- `skewY`, `matrix()`,
/// `scale(1e35, 1.89)` and `perspective() translateZ()` boxes read 1 to 10 float32 ULPs off. `None` when a
/// transform here is one `gfx_transform` does not model.
fn transformed_as_layout(scope: &mut v8::PinScope, node: u32, rects: &[[f64; 4]]) -> Option<Vec<[f64; 4]>> {
    use crate::core::geometry::gfx_transform::Transform;
    use page::layout::style::CssPosition;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let chain: Vec<u32> = {
        let dom = &st.borrow::<HostDom>().0;
        let mut v = Vec::new();
        let mut cur = Some(node);
        while let Some(n) = cur {
            if dom.node_type(n) == 1 {
                v.push(n);
            }
            cur = dom.parent(n).and_then(|p| if dom.is_shadow_root(p) { dom.shadow_host_of(p) } else { Some(p) });
        }
        v
    };
    struct Hop {
        node: u32,
        at: (f64, f64),
        size: (f32, f32),
        transforms: bool,
        position: Option<CssPosition>,
        inline: bool,
    }
    let (hops, scroll) = measure_node(&mut st, node, (Vec::new(), (0.0f64, 0.0f64)), |out| {
        let hops: Vec<Hop> = chain
            .iter()
            .map(|&e| {
                let r = out.viewport_bounding_rect(e);
                Hop {
                    node: e,
                    at: (r.x as f64, r.y as f64),
                    size: (r.width, r.height),
                    transforms: out.transforms(e),
                    position: out.position_of(e),
                    inline: out.is_inline_box(e),
                }
            })
            .collect();
        // the viewport's scroll: where layout put the root, less where the client sees it
        let scroll = chain.last().map_or((0.0, 0.0), |&root| {
            let (d, v) = (out.bounding_rect(root), out.viewport_bounding_rect(root));
            ((d.x - v.x) as f64, (d.y - v.y) as f64)
        });
        (hops, scroll)
    });
    if !hops.iter().any(|h| h.transforms) {
        return Some(rects.to_vec());
    }
    let mut layers: Vec<Option<Transform>> = Vec::with_capacity(hops.len());
    for h in &hops {
        layers.push(if h.transforms { Some(crate::core::css::cssom::layout_transform(&mut st, h.node, h.size)?) } else { None });
    }
    // `LayoutObject::Container`: an absolutely positioned box's is its nearest positioned or transforming ancestor,
    // a fixed one's its nearest transforming ancestor, any other box's its parent; `None` is the view
    let container = |i: usize| -> Option<usize> {
        let next = (i + 1..hops.len()).find(|&j| match hops[i].position {
            Some(CssPosition::Absolute) => hops[j].transforms || !matches!(hops[j].position, Some(CssPosition::Static) | None),
            Some(CssPosition::Fixed) => hops[j].transforms,
            _ => true,
        });
        next
    };
    let at_of = |c: Option<usize>| match c {
        Some(j) => hops[j].at,
        None => (-scroll.0, -scroll.1),
    };
    // A non-atomic INLINE box has no coordinate space of its own: its line boxes sit in its container's, and Blink
    // maps them through ONE accumulated transform (`LayoutInline::QuadsForSelfInternal` ->
    // `LocalToAncestorTransform`): every container offset folded in as a `PostTranslate`, every transform multiplied
    // in and flattened, the quad mapped once -- not the box path's float steps (Chrome 150: the `<span>` in a
    // `skewY()` box reads -11.404540061950684 this way, -11.404541015625 the box way).
    if hops[0].inline {
        let first = container(0);
        let origin = at_of(first);
        let mut accum = Transform::identity();
        let mut i = first;
        while let Some(k) = i {
            let c = container(k);
            let (cx, cy) = at_of(c);
            let off = (hops[k].at.0 - cx, hops[k].at.1 - cy);
            match &layers[k] {
                Some(layer) => {
                    let mut t = Transform::identity();
                    t.pre_concat(layer);
                    t.post_translate(off.0 as f32, off.1 as f32);
                    accum.concat_after(&t);
                    accum.flatten();
                }
                None => accum.post_translate(off.0 as f32, off.1 as f32),
            }
            i = c;
        }
        let out = rects
            .iter()
            .map(|r| {
                let (lx, ly) = ((r[0] - origin.0) as f32, (r[1] - origin.1) as f32);
                let (right, bottom) = (lx + r[2] as f32, ly + r[3] as f32);
                let quad = [(lx, ly), (right, ly), (right, bottom), (lx, bottom)].map(|(x, y)| accum.map_point(x, y));
                let xs = quad.map(|p| p.0);
                let ys = quad.map(|p| p.1);
                let (x0, x1) = (xs.iter().copied().fold(f32::INFINITY, f32::min), xs.iter().copied().fold(f32::NEG_INFINITY, f32::max));
                let (y0, y1) = (ys.iter().copied().fold(f32::INFINITY, f32::min), ys.iter().copied().fold(f32::NEG_INFINITY, f32::max));
                [(x0 - scroll.0 as f32) as f64, (y0 - scroll.1 as f32) as f64, (x1 - x0) as f64, (y1 - y0) as f64]
            })
            .collect();
        return Some(out);
    }
    let out = rects
        .iter()
        .map(|r| {
            // the rect in the box's own coordinates, as `QuadF(RectF)` takes its corners
            let (lx, ly) = ((r[0] - hops[0].at.0) as f32, (r[1] - hops[0].at.1) as f32);
            let (right, bottom) = (lx + r[2] as f32, ly + r[3] as f32);
            let mut quad = [(lx, ly), (right, ly), (right, bottom), (lx, bottom)];
            let mut acc = (0.0f64, 0.0f64);
            let mut i = 0usize;
            loop {
                let c = container(i);
                let (cx, cy) = at_of(c);
                let off = (hops[i].at.0 - cx, hops[i].at.1 - cy);
                match &layers[i] {
                    Some(layer) => {
                        for p in quad.iter_mut() {
                            p.0 += acc.0 as f32;
                            p.1 += acc.1 as f32;
                        }
                        acc = (0.0, 0.0);
                        let mut t = Transform::identity();
                        t.pre_concat(layer);
                        t.post_translate(off.0 as f32, off.1 as f32);
                        for p in quad.iter_mut() {
                            *p = t.map_point(p.0, p.1);
                        }
                    }
                    None => {
                        acc.0 += off.0;
                        acc.1 += off.1;
                    }
                }
                match c {
                    Some(j) => i = j,
                    None => break,
                }
            }
            for p in quad.iter_mut() {
                p.0 += acc.0 as f32;
                p.1 += acc.1 as f32;
            }
            // `QuadF::BoundingBox`, then `AdjustRectForScrollAndAbsoluteZoom` (`rect.Offset(-scroll)`)
            let xs = quad.map(|p| p.0);
            let ys = quad.map(|p| p.1);
            let (x0, x1) = (xs.iter().copied().fold(f32::INFINITY, f32::min), xs.iter().copied().fold(f32::NEG_INFINITY, f32::max));
            let (y0, y1) = (ys.iter().copied().fold(f32::INFINITY, f32::min), ys.iter().copied().fold(f32::NEG_INFINITY, f32::max));
            let (w, h) = (x1 - x0, y1 - y0);
            [(x0 - scroll.0 as f32) as f64, (y0 - scroll.1 as f32) as f64, w as f64, h as f64]
        })
        .collect();
    Some(out)
}

/// [`transformed`] mapped about each transform's origin in viewport coordinates through one double matrix -- the
/// mapping before Blink's layout arithmetic, kept for the transforms `gfx_transform` does not model.
fn transformed_textbook(scope: &mut v8::PinScope, node: u32, rects: Vec<[f64; 4]>) -> Vec<[f64; 4]> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let chain: Vec<u32> = {
        let dom = &st.borrow::<HostDom>().0;
        let mut v = Vec::new();
        let mut cur = Some(node);
        while let Some(n) = cur {
            if dom.node_type(n) == 1 {
                v.push(n);
            }
            cur = dom.parent(n).and_then(|p| if dom.is_shadow_root(p) { dom.shadow_host_of(p) } else { Some(p) });
        }
        v
    };
    // only the boxes the layout says transform: one look per ancestor, no computed style for the rest
    let chain: Vec<u32> = measure_node(&mut st, node, Vec::new(), |out| chain.iter().copied().filter(|&e| out.transforms(e)).collect());
    let mut layers: Vec<([f64; 16], [f64; 3], (f64, f64))> = Vec::new();
    for e in chain {
        let at = measure_node(&mut st, e, None, |out| {
            let r = out.viewport_bounding_rect(e);
            Some((r.x as f64, r.y as f64, r.width as f64, r.height as f64))
        });
        let Some((x, y, w, h)) = at else { continue };
        let Some((m, o)) = crate::core::css::cssom::transform_about_origin(&mut st, e, (w, h)) else { continue };
        layers.push((m, [x + o[0], y + o[1], o[2]], (x, y)));
    }
    if layers.is_empty() {
        return rects;
    }
    // A transform that only scales and translates is Chromium's `AxisTransform2d`: each corner mapped in
    // FLOAT arithmetic, `x * scale + translation`, with the scale and the translation stored as floats and
    // the corner relative to the transformed box (`AxisTransform2d::MapPoint`). Chrome 150: `scale(2,
    // 1.89)` on a 44px-tall box reads 83.16000366 tall (the double mapping gives 83.15997).
    let axis = |m: &[f64; 16]| {
        m[1] == 0.0 && m[4] == 0.0 && [m[2], m[3], m[6], m[7], m[8], m[9], m[11], m[14]].iter().all(|v| *v == 0.0) && m[10] == 1.0 && m[15] == 1.0
    };
    rects
        .into_iter()
        .map(|r| {
            let mut quad = [(r[0], r[1]), (r[0] + r[2], r[1]), (r[0] + r[2], r[1] + r[3]), (r[0], r[1] + r[3])];
            for (m, o, pos) in &layers {
                if axis(m) {
                    let (sx, sy) = (m[0] as f32, m[5] as f32);
                    // the translation of the box's own coordinates: its place, its origin, the matrix's
                    // (stored as floats, unclamped: only a mapped point is clamped)
                    let tx = (pos.0 + m[12] + (o[0] - pos.0) * (1.0 - m[0])) as f32;
                    let ty = (pos.1 + m[13] + (o[1] - pos.1) * (1.0 - m[5])) as f32;
                    for p in quad.iter_mut() {
                        let (lx, ly) = ((p.0 - pos.0) as f32, (p.1 - pos.1) as f32);
                        *p = (clamp_float_geometry((lx * sx + tx) as f64), clamp_float_geometry((ly * sy + ty) as f64));
                    }
                    continue;
                }
                for p in quad.iter_mut() {
                    let (x, y, z) = (p.0 - o[0], p.1 - o[1], -o[2]);
                    let tx = m[0] * x + m[4] * y + m[8] * z + m[12];
                    let ty = m[1] * x + m[5] * y + m[9] * z + m[13];
                    // w as a float (measured: Chrome's `perspective()` corners are the float w's)
                    let w = (m[3] * x + m[7] * y + m[11] * z + m[15]) as f32 as f64;
                    let (px, py) = if w != 1.0 && w.is_normal() { (tx / w, ty / w) } else { (tx, ty) };
                    *p = (clamp_float_geometry(px + o[0]), clamp_float_geometry(py + o[1]));
                }
            }
            let xs = quad.map(|p| p.0 as f32);
            let ys = quad.map(|p| p.1 as f32);
            let (x0, x1) = (xs.iter().copied().fold(f32::INFINITY, f32::min), xs.iter().copied().fold(f32::NEG_INFINITY, f32::max));
            let (y0, y1) = (ys.iter().copied().fold(f32::INFINITY, f32::min), ys.iter().copied().fold(f32::NEG_INFINITY, f32::max));
            [x0 as f64, y0 as f64, (x1 - x0) as f64, (y1 - y0) as f64]
        })
        .collect()
}

/// Chromium's `ClampFloatGeometry`: a mapped coordinate within +-`f32::MAX / 1e6`, and 0 for NaN, so a
/// `scale(1e35)` box reads +-3.4028e32 wide rather than infinity.
fn clamp_float_geometry(v: f64) -> f64 {
    let max = (f32::MAX / 1e6) as f64;
    if v.is_nan() {
        0.0
    } else {
        v.clamp(-max, max)
    }
}

/// `getClientRects()`: a `DOMRectList` -- a platform object, not an Array (`Array.isArray` is FALSE
/// in every browser and the value is `instanceof DOMRectList`). A fresh one per call, which is what
/// Chrome answers: it is the one list on this surface that is NOT `[SameObject]`.
pub(crate) fn cb_el_client_rects(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let node = node_of(scope, &args);
    let rects = node.map(|n| client_rects_of(scope, n)).unwrap_or_default();
    let arr = v8::Array::new(scope, rects.len() as i32);
    for (i, r) in rects.into_iter().enumerate() {
        let vals = rect_args(scope, r);
        if let Some(o) = new_global(scope, "DOMRect", &vals) {
            arr.set_index(scope, i as u32, o.into());
        }
    }
    let list = static_list_reuse(scope, "DOMRectList", arr, None);
    rv.set(list);
}

/// Element `node`'s client rects, `[x, y, width, height]`, through the transforms above it.
pub(crate) fn client_rects_of(scope: &mut v8::PinScope, node: u32) -> Vec<[f64; 4]> {
    let (rects, transforms): (Vec<[f64; 4]>, bool) = measured_with_dom(scope, Some(node), (Vec::new(), false), |out, dom, n| {
        let rects = out.viewport_client_rects(n).into_iter().map(|r| [r.x as f64, r.y as f64, r.width as f64, r.height as f64]).collect();
        (rects, any_transform(out, dom, n))
    });
    if transforms { transformed(scope, node, rects) } else { rects }
}

/// Text node `node`'s rects from UTF-16 offset `start` to `end` (`page::LaidOut::text_range_rects`), through the
/// transforms above it -- a `Range`'s rects for that text.
pub(crate) fn text_range_rects_of(scope: &mut v8::PinScope, node: u32, start: u32, end: u32) -> Vec<[f64; 4]> {
    let (rects, parent): (Vec<[f64; 4]>, Option<u32>) = measured_with_dom(scope, Some(node), (Vec::new(), None), |out, dom, n| {
        let rects = out.text_range_rects(dom, n, start, end).into_iter().map(|r| [r.x as f64, r.y as f64, r.width as f64, r.height as f64]).collect();
        let parent = dom.parent(n).filter(|&p| dom.node_type(p) == 1 && any_transform(out, dom, p));
        (rects, parent)
    });
    match parent {
        Some(p) => transformed(scope, p, rects),
        None => rects,
    }
}

/// The integer metric rows, by index into one layout read: `[offsetWidth, offsetHeight, offsetLeft,
/// offsetTop, clientWidth, clientHeight, clientTop, clientLeft]`. One layout read rather than one per
/// member, because a page that reads `offsetWidth` reads `offsetHeight` next.
///
/// `clientTop`/`clientLeft` USED TO BE DERIVED here, as `(offsetHeight - clientHeight) / 2` -- the
/// border box less the padding box, halved. That is only the border width when the two borders on an
/// axis are equal and nothing else eats into the padding box, and it stopped being true at all when
/// `clientHeight` began answering the VIEWPORT for the root element: on a 50px-tall
/// page the derivation reads `(50 - 993) / 2 = -471` where Chrome answers 0. They are the used
/// border widths now, straight from the layout.
pub(crate) const METRICS: [&str; 8] = ["offsetWidth", "offsetHeight", "offsetLeft", "offsetTop", "clientWidth", "clientHeight", "clientTop", "clientLeft"];

/// The row's index into [`METRICS`] (`-1` for a name that is not one), the getter's hidden data.
pub(crate) fn metric_index(name: &str) -> i32 {
    METRICS.iter().position(|m| *m == name).map(|i| i as i32).unwrap_or(-1)
}

/// The metric getter body; `args.data()` is the [`METRICS`] index.
pub(crate) fn cb_metric<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let index = crate::binding::protos::body_data(scope, &args).int32_value(scope).unwrap_or(-1);
    let node = node_of(scope, &args);
    // `offsetLeft`/`offsetTop` are measured from the element `offsetParent` answers
    let parent = match (index, node) {
        (2 | 3, Some(n)) => offset_parent_id(scope, n),
        _ => None,
    };
    // Only the pair the row asks for: each is a function of the layout alone.
    let m = measured(scope, node, 0i32, |out, n| {
        let pair = |(a, b): (i32, i32), second: bool| if second { b } else { a };
        match index {
            0 | 1 => pair(out.offset_size(n), index == 1),
            2 | 3 => pair(out.offset_position_from(n, parent), index == 3),
            4 | 5 => pair(out.client_size(n), index == 5),
            6 | 7 => pair(out.border_widths(n), index == 7),
            _ => 0,
        }
    });
    rv.set_int32(m);
}

/// `offsetParent` (CSSOM View §7) of element `id`, read off the layout rather than off computed-style
/// objects -- every `offsetLeft`/`offsetTop` asks it, and building a declaration per ancestor made one
/// read 23.5 us where Chrome takes 0.45. `None` (null) for an element with no box, `<body>`, the root, a
/// fixed box, and one no ancestor qualifies for; otherwise the nearest ancestor that is positioned, is
/// `<body>`, or -- for a box that is itself static -- is a `td`, `th` or `table`.
pub(crate) fn offset_parent_id(scope: &mut v8::PinScope, id: u32) -> Option<u32> {
    if !crate::core::html::subresource::connected_anywhere(scope, id) {
        return None;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let is = |dom: &korvus_dom::Dom, n: u32, tag: &str| dom.tag_name(n).is_some_and(|t| t.eq_ignore_ascii_case(tag));
    let element = |dom: &korvus_dom::Dom, n: Option<u32>| n.filter(|&p| dom.node_type(p) == 1);
    // The root and `<body>` answer null before any layout is asked for, as they always have.
    {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        if is(dom, id, "html") || (is(dom, id, "body") && element(dom, dom.parent(id)).is_some_and(|p| is(dom, p, "html"))) {
            return None;
        }
    }
    // One walk up the element ancestors, nearest first, asking the layout about each only until one qualifies: it
    // built an upper-cased tag name per ancestor and read every ancestor's position before testing any (1.0-1.15 us
    // an `offsetTop` read against Chrome's 0.45, 2026-10-23).
    let mut st = op_state.borrow_mut();
    measure_node_with_dom(&mut st, id, None, |out, dom| {
        use page::layout::style::CssPosition;
        let own = out.position_of(id)?;
        if own == CssPosition::Fixed {
            return None;
        }
        let mut cur = element(dom, dom.parent(id));
        while let Some(p) = cur {
            let up = element(dom, dom.parent(p));
            // `<body>` is the root's body child
            let body = is(dom, p, "body") && up.is_some_and(|r| is(dom, r, "html") && element(dom, dom.parent(r)).is_none());
            if body
                || out.position_of(p).is_some_and(|x| x != CssPosition::Static)
                || (own == CssPosition::Static && (is(dom, p, "td") || is(dom, p, "th") || is(dom, p, "table")))
            {
                return Some(p);
            }
            cur = up;
        }
        None
    })
}

/// `scrollWidth` / `scrollHeight`: the size of the element's scrolling box. `args.data()` is 1 for
/// the height and 0 for the width.
///
/// This was `cb_zero_uint`, a literal `0` under a comment that said "the scrolling box's size, which
/// has no layout behind it here" -- one line above the row where `clientWidth`/`clientHeight` were
/// already going through [`cb_metric`] to the same layout. A measurement found what that cost:
/// `window.scrollTo(0, 2e6)` left `scrollY` at 2000000 on 797 corpus pages whose
/// `documentElement.scrollHeight` read 0, and the interactivity axis's `scroll` capability -- 586
/// missing floor rows, its largest cluster -- is defined as "the document moved AND landed at its
/// own `scrollHeight - innerHeight`" precisely because a zero here makes that unsatisfiable.
///
/// The receiver check is [`crate::core::dom::element::el_this`] and not [`node_of`], because the
/// zero it replaces threw `TypeError: Illegal invocation` for a foreign receiver and that half was
/// right: Chrome throws from this getter too.
pub(crate) fn cb_scroll_size(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let height = crate::binding::protos::body_data(scope, &args).boolean_value(scope);
    if crate::core::dom::element::el_this(scope, &args).is_none() {
        return;
    }
    let node = node_of(scope, &args);
    let (w, h) = measured(scope, node, (0, 0), |out, n| out.scroll_size(n));
    rv.set_int32(if height { h } else { w });
}

/// How far an element can be scrolled on each axis, for the `scrollTop`/`scrollLeft` clamp.
///
/// `(0, 0)` for an element with no box, for one that is not a scroll container, and for a build with
/// no page plugin at all -- in each of which nothing scrolls and the honest extent is zero.
pub(crate) fn element_scroll_extent(state: &mut OpState, node: u32) -> (f64, f64) {
    let (x, y) = measure_node(state, node, (0, 0), |out| out.scroll_extent(node));
    (x as f64, y as f64)
}

/// An element's border box in VIEWPORT coordinates, `getBoundingClientRect`'s numbers -- the origin an
/// outermost `<svg>`'s `getScreenCTM` starts from.
pub(crate) fn element_viewport_rect(state: &mut OpState, node: u32) -> (f64, f64, f64, f64) {
    measure_node(state, node, (0.0, 0.0, 0.0, 0.0), |out| {
        let r = out.viewport_bounding_rect(node);
        (r.x as f64, r.y as f64, r.width as f64, r.height as f64)
    })
}

/// A scroll container's scrollport in VIEWPORT coordinates -- its padding box less its scrollbars, what
/// `scrollIntoView` aligns its target within. `None` for an element with no box.
pub(crate) fn element_viewport_padding_box(state: &mut OpState, node: u32) -> Option<(f64, f64, f64, f64)> {
    measure_node(state, node, None, |out| {
        out.viewport_padding_box(node).map(|r| (r.x as f64, r.y as f64, r.width as f64, r.height as f64))
    })
}

/// The size of an element's content box -- `(0, 0)` for one with no box. A frame window's `innerWidth`
/// and `innerHeight`.
/// The element's laid-out CONTENT box, or `None` when it generates no box (it is not being rendered).
pub(crate) fn element_content_box(state: &mut OpState, node: u32) -> Option<(f64, f64)> {
    measure_node(state, node, None, |out| out.content_box(node).map(|r| (r.width as f64, r.height as f64)))
}

pub(crate) fn element_content_size(state: &mut OpState, node: u32) -> (f64, f64) {
    measure_node(state, node, (0.0, 0.0), |out| out.content_box(node).map_or((0.0, 0.0), |r| (r.width as f64, r.height as f64)))
}

/// How far the DOCUMENT can be scrolled -- what `window.scrollTo(0, 2e6)` lands on.
///
/// The root element's own extent, because the viewport's scrolling box *is* the root's. Measured on
/// Chrome 150 (2026-09-13): on a page whose `documentElement.scrollHeight` is 4300 in a 993px
/// viewport, `scrollTo(0, 2e6)` and `documentElement.scrollTop = 2e6` both land on 3307, and each is
/// visible through the other.
pub(crate) fn document_scroll_extent(state: &mut OpState) -> (f64, f64) {
    let (x, y) = measure(state, (0, 0), |out| out.document_scroll_extent());
    (x as f64, y as f64)
}

/// `Page.getLayoutMetrics`' numbers: `[client width, client height, scrollX, scrollY, content width, content height]`
/// in CSS px -- the layout viewport as `documentElement.clientWidth`/`clientHeight` read it, where it is scrolled to,
/// and the document's scroll size (`documentElement.scrollWidth`/`scrollHeight`: the viewport plus how far it scrolls).
pub fn layout_metrics(scope: &mut v8::PinScope) -> [f64; 6] {
    let (cw, ch) = viewport_client_size(scope);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let (sx, sy) = crate::core::html::window_geometry::scroll_position(&st);
    let (ex, ey) = document_scroll_extent(&mut st);
    [cw, ch, sx, sy, cw + ex, ch + ey]
}

/// THE LAYOUT VIEWPORT'S SIZE — the document element's client area, which is the SAME layout read
/// `documentElement.clientWidth` / `clientHeight` answer from ([`cb_metric`], indices 4 and 5).
///
/// It exists so `visualViewport.width`/`height` can be that number rather than a second one. Measured
/// on Chrome 150 and Brave 151: on a scrolling 1920x1080 window,
/// `visualViewport.width`/`height` read **1904 / 976** beside an `innerWidth`/`innerHeight` of
/// **1919 / 991** and a `documentElement.clientWidth`/`clientHeight` of **1904 / 976**. The two
/// viewport objects disagree on purpose and `core/html/bar_props.rs` records the other half:
/// `viewport.segments[0]` is the INNER box. Reading `innerWidth` here would swap one tell for another.
///
/// `node` is the document element, or the media context's viewport when this build has no layout at
/// all — the same fallback `IntersectionObserver`'s own viewport read takes, for the same reason.
pub(crate) fn viewport_client_size(scope: &mut v8::PinScope) -> (f64, f64) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let de = crate::core::dom::element::document_element_id(&op_state);
    let mut st = op_state.borrow_mut();
    let media = media_context(&st);
    let fallback = (media.viewport_width as f64, media.viewport_height as f64);
    let Some(de) = de else { return fallback };
    let (w, h) = measure_node(&mut st, de, (0, 0), |out| out.client_size(de));
    if w == 0 && h == 0 {
        return fallback;
    }
    (w as f64, h as f64)
}

// ── hit testing: the two `Document` rows ─────────────────────────────────────────────
//
// `document.elementFromPoint(x, y)` did not exist at all until the kernel's `BOOTSTRAP` string grew a
// block for it: two `KB.__*` bridge slots wrapped in `op_helper_method` + `op_arity_guard`, defining
// two members straight onto `Document.prototype` from a Rust string literal. These are those bodies,
// as the Document table's own rows -- same answers, one owner, no mask, no bridge.
// Both `op_helper_method` and `op_arity_guard` were deleted once nothing named them;
// the guard the rows still get is built in Rust by `installer::make_arity_guard`.
//
// THE BOX TREE BELONGS TO THE PAGE'S DOCUMENT. A document with no browsing context was never laid
// out, so it has no box at any coordinate: the recorded browser answers `null` (and an empty list)
// for `new Document().elementFromPoint(1, 1)`, where hit-testing the page's tree on its behalf would
// hand back the page's `<html>`. The receiver test is the same one `cb_doc_current_script` and
// `cb_doc_custom_element_registry` use.
//
// No pixels are involved: this is a question about boxes, so a transparent element on top of a button
// still answers as the element on top. That is why a headless build can answer it at all.
//
// Both arguments are REQUIRED, and the arity check is the table row's (`required 2`), which
// `method_row` turns into the same native `make_arity_guard` every other table method gets. The
// deleted glue needed `op_arity_guard` explicitly because `op_helper_method` could not see
// `arguments.length` -- so `document.elementFromPoint()` hit-tested (0, 0) and answered with whatever
// covers the origin, where a browser throws.
/// The arena root of the document `this` is, when it is one a browsing context lays out: the page's
/// (answered as the page root), or a frame's. `None` for any other document.
fn laid_out_document(scope: &mut v8::PinScope, this: v8::Local<v8::Value>) -> Option<u32> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if crate::binding::protos::document_val(scope, &op_state).is_some_and(|d| d == this) {
        let st = op_state.borrow();
        return Some(st.try_borrow::<HostDom>()?.0.root());
    }
    let root = crate::core::dom::shared::document_node_id(scope, this)?;
    let st = op_state.borrow();
    crate::core::html::browsing_context::frame_element_showing(&st, root).map(|_| root)
}

/// The `(x, y)` a hit test is asked about: `+x || 0`, i.e. a non-number (or NaN) is the origin.
fn point(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> (f32, f32) {
    let n = |v: v8::Local<v8::Value>, scope: &mut v8::PinScope| {
        let f = v.number_value(scope).unwrap_or(0.0);
        if f.is_finite() { f as f32 } else { 0.0 }
    };
    let x = n(args.get(0), scope);
    let y = n(args.get(1), scope);
    (x, y)
}

pub(crate) fn cb_doc_element_from_point(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(root) = laid_out_document(scope, args.this().into()) else {
        rv.set_null();
        return;
    };
    let (x, y) = point(scope, &args);
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let id = {
        let mut st = op_state.borrow_mut();
        element_from_point(&mut st, root, x, y)
    };
    if id < 0 {
        rv.set_null();
        return;
    }
    let el = crate::binding::protos::wrap_cached(scope, &op_state, id as u32);
    rv.set(el.into());
}

pub(crate) fn cb_doc_elements_from_point(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let ids: Vec<u32> = if let Some(root) = laid_out_document(scope, args.this().into()) {
        let (x, y) = point(scope, &args);
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let mut st = op_state.borrow_mut();
        elements_from_point(&mut st, root, x, y)
    } else {
        Vec::new()
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let arr = v8::Array::new(scope, ids.len() as i32);
    for (i, id) in ids.into_iter().enumerate() {
        let el = crate::binding::protos::wrap_cached(scope, &op_state, id);
        arr.set_index(scope, i as u32, el.into());
    }
    rv.set(arr.into());
}

/// `document.caretPositionFromPoint(x, y)` -- a real `CaretPosition`, or `null` outside the viewport
/// and for a document nothing lays out (see `page::layout::caret`).
///
/// It was a null for every point: "a caret position needs inline text flow, and that layer does not
/// exist". It exists (P3), and Chrome answers `(#a, 0)` on a box with no text, `(text, 4)` between
/// `hell` and `o`, and `(text, 11)` past the end of `hello world`.
pub(crate) fn cb_doc_caret_position_from_point(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !crate::binding::protos::require_args(scope, &args, 2, "Document", "caretPositionFromPoint") {
        return;
    }
    // `optional CaretPositionFromPointOptions options = {}`, converted after the two coordinates (Chrome 150):
    // `shadowRoots` read once, a sequence of ShadowRoots; a non-object is no dictionary
    let (x, y) = point(scope, &args);
    if crate::install::ctor_dicts::convert_dict(scope, args.get(2), "CaretPositionFromPointOptions", "Failed to execute 'caretPositionFromPoint' on 'Document': ").is_err() {
        return;
    }
    let Some(root) = laid_out_document(scope, args.this().into()) else {
        rv.set_null();
        return;
    };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let caret = {
        let mut st = op_state.borrow_mut();
        // Bring the document's layout up to date, then read it beside the DOM: a caret offset is an
        // offset into the DOM's text, not the laid-out copy.
        measure_node(&mut st, root, (), |_| ());
        let dom = match st.try_borrow::<HostDom>() {
            Some(d) => &d.0,
            None => {
                drop(st);
                rv.set_null();
                return;
            }
        };
        if dom.root() == root {
            match st.try_borrow::<LayoutCache>() {
                Some(LayoutCache(Some((_, out)))) => out.caret_position(dom, x, y),
                _ => None,
            }
        } else {
            st.try_borrow::<FrameLayouts>().and_then(|f| f.frames.get(&root)).and_then(|(_, out)| out.caret_position(dom, x, y))
        }
    };
    let Some(caret) = caret else {
        rv.set_null();
        return;
    };
    let Some(proto) = crate::core::intersection_observer::rendering::interface_proto(scope, "CaretPosition") else {
        rv.set_null();
        return;
    };
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    let node: v8::Local<v8::Value> = crate::binding::protos::wrap_cached(scope, &op_state, caret.node).into();
    crate::core::events::event::evt_slot_write(scope, obj, "offsetNode", node);
    crate::core::events::event::evt_slot_write_num(scope, obj, "offset", caret.offset as f64);
    let r = caret.rect;
    let parts = v8::Array::new(scope, 4);
    for (i, n) in [r.x, r.y, r.width, r.height].into_iter().enumerate() {
        let v = v8::Number::new(scope, n as f64);
        parts.set_index(scope, i as u32, v.into());
    }
    crate::core::events::event::evt_slot_write(scope, obj, "clientRect", parts.into());
    crate::install::webidl::brand(scope, obj, "CaretPosition");
    rv.set(obj.into());
}

/// `CaretPosition.prototype.getClientRect()`: a fresh `DOMRect` of the caret -- zero wide, as tall as
/// the text's content area -- and all zeros for a position that is not in text (Chrome 150).
fn cb_caret_client_rect(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    let mut r = [0.0f64; 4];
    if let Some(parts) = crate::core::events::event::evt_slot_read(scope, this, "clientRect").and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        for (i, slot) in r.iter_mut().enumerate() {
            *slot = parts.get_index(scope, i as u32).and_then(|v| v.number_value(scope)).unwrap_or(0.0);
        }
    }
    let vals = rect_args(scope, r);
    if let Some(o) = new_global(scope, "DOMRect", &vals) {
        rv.set(o.into());
    }
}

/// The `caret_position` kit's rows (`install/kits.rs`).
pub(crate) fn install_caret_position(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    match member.name {
        "offsetNode" | "offset" => crate::install::kits::evt_get(scope, proto, member),
        "getClientRect" => crate::install::webidl::branded_method_row(scope, proto, member, cb_caret_client_rect),
        other => debug_assert!(false, "install_caret_position: unexpected member {other}"),
    }
}

// This file declares no ops: every row is a Rust callback over the fns above.
