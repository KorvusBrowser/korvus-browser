//! `core/html/lazy_load.rs` -- `loading=lazy`: an `<img>` or `<iframe>` is requested only once it is
//! NEAR THE VIEWPORT (L17's E3).
//!
//! THE DEFECT. The element loader requested every lazy image and frame the moment it saw one. Measured
//! on Chrome 150.0.7871.46 over a loopback server that logs paths, headless and headed under Xvfb
//! agreeing on every row:
//!
//! ```text
//! <img loading=lazy> 6000px below the fold             korvus fetched       chrome not
//! <img loading=lazy> in the viewport                   both fetch it, chrome +5..20 ms AFTER `load`
//! scrollTo(0, 6000) with lazy content at 3000/6000/9000   chrome fetches the 6000 row within ~2 ms
//! display:none lazy image, or one in a display:none parent   chrome never fetches it
//! visibility:hidden lazy image in the viewport          chrome fetches it
//! `new Image()` with loading=lazy and a src, never inserted   chrome never fetches it
//! loading=LAZY / eager / bogus                          deferred / fetched / fetched
//! ```
//!
//! # How near is near -- measured, then found in Blink
//!
//! Chrome watches every deferred element with an `IntersectionObserver` whose SCROLL MARGIN widens the
//! viewport and every scroll container on the way up. Bisected on 10 px grids of absolutely positioned
//! 10x10 elements (the body's 8 px margin included), with the connection reporting `4g`:
//!
//! ```text
//! image, below the viewport            loads when top < innerHeight + 1250
//! image, right of the viewport         loads when left < width + 625       (half)
//! image, inside a 500x300 overflow:auto / overflow:hidden box   the same 1250 / 625, from the BOX's edge
//! iframe, below the viewport (and in a scroller)                2500
//! the same page at --force-device-scale-factor=2                unchanged (CSS pixels)
//! ```
//!
//! Blink says the same, and says why it is a table: `LazyLoadMediaObserver` builds its observer with
//! `scroll_margin = {margin (top & bottom), margin / 2 (right & left)}` and `LazyLoadFrameObserver`
//! with `{margin}` on all four sides, where `margin` is chosen by the EFFECTIVE CONNECTION TYPE from the
//! renderer settings (`settings.json5`, `lazyLoading{Image,Frame}MarginPx*`). The 4g row is the one
//! measured above; the other rows are those settings' values for the same build.
//!
//! WHICH PERSONA VALUE DRIVES IT: `navigator.connection.effectiveType` -- the bundle's
//! `nav.connection.effectiveType` (`client_hints().connection.effective_type`). A device that reports
//! `3g` gets the 3g margins, exactly as the page's own `navigator.connection` would predict; nothing
//! here asks which browser the persona is. A persona with no `connection` at all (Brave's capture:
//! the browser hides the API) takes the kernel's default connection, `4g`, not Blink's "unknown" row.
//!
//! # When the check runs
//!
//! At the document's `load` event (Chrome's lazy fetches land a few ms after it -- the first frame),
//! and after that whenever something that moves the answer happened: at the end of the task that
//! scrolled the document or an element, on a one-shot frame when a deferred element was added or
//! the DOM changed shape, and at every rendering opportunity the runtime offers ([`opportunity`]). There is no
//! compositor here, so "near the viewport" is asked of the same `korvus-page` layout that answers
//! `getBoundingClientRect`.
//!
//! # Rulings
//!
//! * An image or frame in a NESTED document is loaded eagerly: nested documents are not laid out here,
//!   so there is no viewport for them to be near. Cost if wrong: a lazy image deep inside an iframe is
//!   requested where Chrome waits.
//! * The viewport is the window's inner size (`innerWidth` x `innerHeight`), scrollbars included.
//!   Cost if wrong: an element within 15 px of the threshold on a page with a classic scrollbar.
//! * `overflow: clip` is treated like `visible` (it clips, but it is not a scroll container, so no
//!   scroll margin applies and the element is measured against the next container out).

use std::collections::HashMap;

use deno_core::{v8, OpState};

use crate::binding::protos::build_fn_data;
use crate::binding::HostDom;

/// What a deferred element is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Image,
    Frame,
}

/// This document's deferred elements, in the order they were deferred.
#[derive(Default)]
pub(crate) struct LazyLoads {
    pending: Vec<(u32, Kind)>,
    /// The document's `load` event has fired: a check now runs on the next frame, not at `load`.
    loaded: bool,
    frame_armed: bool,
    scroll_queued: bool,
    /// Bumped per document, so a frame that outlives its document does nothing.
    epoch: u32,
    /// Moved whenever `pending` changes ([`defer`], [`forget`], a check that found something due): part of
    /// [`check`]'s key.
    gen: u64,
    /// What the last check that ran was a function of ([`check`]): an equal key finds nothing due again.
    last_key: Option<u64>,
}

fn with<R>(st: &mut OpState, f: impl FnOnce(&mut LazyLoads) -> R) -> R {
    if st.try_borrow::<LazyLoads>().is_none() {
        st.put(LazyLoads::default());
    }
    f(st.borrow_mut::<LazyLoads>())
}

fn with_scope<R>(scope: &mut v8::PinScope, f: impl FnOnce(&mut LazyLoads) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    with(&mut st, f)
}

/// A new document: nothing is deferred and `load` has not fired.
pub(crate) fn reset(op_state: &std::rc::Rc<std::cell::RefCell<OpState>>) {
    let mut st = op_state.borrow_mut();
    with(&mut st, |l| {
        let epoch = l.epoch.wrapping_add(1);
        *l = LazyLoads { epoch, ..LazyLoads::default() };
    });
}

/// Defer `id`'s load until it is near the viewport.
pub(crate) fn defer(scope: &mut v8::PinScope, id: u32, kind: Kind) {
    let loaded = with_scope(scope, |l| {
        if !l.pending.iter().any(|(p, _)| *p == id) {
            l.pending.push((id, kind));
            l.gen += 1;
        }
        l.loaded
    });
    if loaded {
        arm(scope);
    }
}

/// `id` no longer waits (it loaded eagerly, or no longer selects a source).
pub(crate) fn forget(scope: &mut v8::PinScope, id: u32) {
    with_scope(scope, |l| {
        l.pending.retain(|(p, _)| *p != id);
        l.gen += 1;
    });
}

/// The page document's `load` event has fired: the first look at the viewport.
pub(crate) fn document_loaded(scope: &mut v8::PinScope) {
    with_scope(scope, |l| l.loaded = true);
    check(scope);
}

/// The tree changed shape: an element may have moved toward the viewport. Looked at on the next frame.
pub(crate) fn invalidated(scope: &mut v8::PinScope) {
    let go = with_scope(scope, |l| l.loaded && !l.pending.is_empty());
    if go {
        arm(scope);
    }
}

/// A rendering opportunity: the deferred elements are looked at again, as Blink's lazy-load `IntersectionObserver`
/// looks at its targets every frame. A change that brings one near arms no check of its own -- a production site's banner
/// carousel shows each slide by turning its `style` from `display: none` to `display: block`, and a list below the
/// fold moves up once the images above it have sizes -- and such images were never requested. The check's key makes an
/// opportunity where nothing moved cost a hash.
pub(crate) fn opportunity(scope: &mut v8::PinScope) {
    let go = with_scope(scope, |l| l.loaded && !l.pending.is_empty());
    if go {
        check(scope);
    }
}

/// The document or an element scrolled. Looked at when the task that scrolled ends (a microtask), so a
/// script that scrolls in a loop costs one look, and the requests go out before anything else runs --
/// Chrome's arrive within ~2 ms of `scrollTo(0, 6000)` (measured), inside the frame the scroll made.
pub(crate) fn scrolled(scope: &mut v8::PinScope) {
    let go = with_scope(scope, |l| l.loaded && !l.pending.is_empty() && !std::mem::replace(&mut l.scroll_queued, true));
    if !go {
        return;
    }
    let epoch = with_scope(scope, |l| l.epoch);
    let data: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, epoch).into();
    let f = build_fn_data(scope, cb_scrolled, "", 0, data);
    scope.enqueue_microtask(f);
}

fn cb_scrolled(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(epoch) = crate::binding::protos::body_data(scope, &args).uint32_value(scope) else { return };
    let current = with_scope(scope, |l| {
        l.scroll_queued = false;
        l.epoch == epoch
    });
    if current {
        check(scope);
    }
}

/// One frame from now, look again. At most one frame is outstanding; it is a real (ref'd) wait, so an
/// engine pumped to idle still reaches it.
fn arm(scope: &mut v8::PinScope) {
    let epoch = with_scope(scope, |l| (!std::mem::replace(&mut l.frame_armed, true)).then_some(l.epoch));
    let Some(epoch) = epoch else { return };
    let Some(f) = crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_css_frame") else {
        with_scope(scope, |l| l.frame_armed = false);
        return;
    };
    // One frame of a 60 Hz display: when Chrome would next run its intersection observations.
    let ms: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, 16).into();
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let Some(p) = f.call(scope, undef, &[ms]).and_then(|v| v8::Local::<v8::Promise>::try_from(v).ok()) else {
        with_scope(scope, |l| l.frame_armed = false);
        return;
    };
    let data: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, epoch).into();
    let tick = build_fn_data(scope, cb_frame, "", 1, data);
    p.then2(scope, tick, tick);
}

fn cb_frame(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(epoch) = crate::binding::protos::body_data(scope, &args).uint32_value(scope) else { return };
    let current = with_scope(scope, |l| {
        if l.epoch == epoch {
            l.frame_armed = false;
            true
        } else {
            false
        }
    });
    if current {
        check(scope);
    }
}

/// The scroll margins, in CSS px: `(image, frame)`, by effective connection type. Blink
/// `settings.json5` `lazyLoadingImageMarginPx*` / `lazyLoadingFrameMarginPx*`; the `4g` row is the
/// measured one (module doc).
fn margins(effective_type: &str) -> (f32, f32) {
    match effective_type {
        "4g" => (1250.0, 2500.0),
        "3g" => (2500.0, 3500.0),
        "2g" => (6000.0, 6000.0),
        "slow-2g" => (8000.0, 8000.0),
        "offline" => (8000.0, 8000.0),
        _ => (3000.0, 4000.0),
    }
}

#[derive(Clone, Copy, Debug)]
struct Box2 {
    l: f32,
    t: f32,
    r: f32,
    b: f32,
}

impl Box2 {
    /// The observer's threshold is the smallest positive float (Blink), so a target that only TOUCHES
    /// the widened edge -- ratio 0 -- is not near. Measured: a 10px image whose top is exactly 1250px
    /// below a 300px scroller's bottom edge is not requested. A zero-area target has no ratio to
    /// speak of and is near when it touches.
    fn intersect(self, o: Box2) -> Option<Box2> {
        let b = Box2 { l: self.l.max(o.l), t: self.t.max(o.t), r: self.r.min(o.r), b: self.b.min(o.b) };
        let empty = self.r <= self.l || self.b <= self.t;
        let ok = if empty { b.l <= b.r && b.t <= b.b } else { b.l < b.r && b.t < b.b };
        ok.then_some(b)
    }
    fn grow(self, v: f32, h: f32) -> Box2 {
        Box2 { l: self.l - h, t: self.t - v, r: self.r + h, b: self.b + v }
    }
}

/// The scroll containers between element `id` and the viewport, innermost first: the elements whose computed
/// `overflow` is `auto`, `scroll` or `hidden`, less the `<html>` and `<body>` whose overflow is the viewport's (ruling: so
/// `body { overflow-x: hidden }` clips nothing but the viewport). `overflow_cache` lives for one check.
fn scrollers(st: &mut OpState, id: u32, overflow_cache: &mut HashMap<u32, bool>) -> Vec<u32> {
    let ancestors = {
        let Some(host) = st.try_borrow::<HostDom>() else { return Vec::new() };
        let dom = &host.0;
        let mut ancestors = Vec::new();
        let mut cur = dom.parent(id);
        while let Some(a) = cur {
            if dom.node_type(a) == 1 {
                ancestors.push(a);
            }
            cur = dom.parent(a);
        }
        ancestors
    };
    let n = ancestors.len();
    ancestors
        .into_iter()
        .take(n.saturating_sub(2))
        .filter(|&a| {
            *overflow_cache.entry(a).or_insert_with(|| {
                ["overflow-x", "overflow-y"].iter().any(|p| {
                    let v = crate::core::css::cssom::css_computed_one(st, a, p);
                    matches!(v.as_str(), "auto" | "scroll" | "hidden")
                })
            })
        })
        .collect()
}

fn box2(r: [f64; 4]) -> Box2 {
    Box2 { l: r[0] as f32, t: r[1] as f32, r: (r[0] + r[2]) as f32, b: (r[1] + r[3]) as f32 }
}

/// Is the element near the viewport, by the IntersectionObserver geometry the module doc describes? Measured in client
/// coordinates through the transforms above it (`getBoundingClientRect`'s geometry): its border box, cut by each scroll
/// container's padding box widened by the margins, then the viewport widened by them. No box (`display: none`, or inside
/// such a subtree) is never near. `clip_cache` lives for one check.
fn near(scope: &mut v8::PinScope, id: u32, scrollers: &[u32], v: f32, h: f32, viewport: Box2, clip_cache: &mut HashMap<u32, Option<Box2>>) -> bool {
    let Some((border, _)) = crate::core::dom::geometry_queries::transformed_boxes(scope, id) else { return false };
    let mut target = box2(border);
    for &a in scrollers {
        let clip = match clip_cache.get(&a) {
            Some(c) => *c,
            None => {
                let c = crate::core::dom::geometry_queries::transformed_boxes(scope, a).and_then(|(_, pad)| pad).map(box2);
                clip_cache.insert(a, c);
                c
            }
        };
        let Some(clip) = clip else { continue };
        match target.intersect(clip.grow(v, h)) {
            Some(t) => target = t,
            None => return false,
        }
    }
    target.intersect(viewport.grow(v, h)).is_some()
}

/// Look at every deferred element once and load the ones near the viewport.
pub(crate) fn check(scope: &mut v8::PinScope) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let due = {
        let mut st = op_state.borrow_mut();
        let pending = with(&mut st, |l| l.pending.clone());
        // The layout is brought up to date even when no deferred element is connected: the refresh hands the page
        // the images that settled since the last layout (`push_natural_images`), which moves the layout key the
        // observers' frames are armed on -- and an image's arrival arms no frame of its own. Skipping it while
        // every deferred element sat in a detached tree (`new Image()`, a React render phase) lost the
        // ResizeObserver entry for an `<img>` whose bytes landed after the first frames -- the box-size change
        // Chrome reports (2026-10-23, a 3000x3000 PNG: the base tree delivered `3000x3000` after `0x0`, the skip
        // nothing more; `observer_delivery_live::a_detached_lazy_image_does_not_hold_back_a_settled_image_s_resize`).
        if pending.is_empty() || !crate::core::dom::refresh_layout_cache(&mut st) {
            return;
        }
        // Only a connected element can be due: with none, no element is looked at.
        if !st.try_borrow::<HostDom>().is_some_and(|h| pending.iter().any(|(id, _)| h.0.is_connected(*id))) {
            return;
        }
        let effective = crate::binding::protos::persona_handle(&st)
            .map(|p| p.0.client_hints().connection.effective_type)
            .filter(|t| !t.is_empty())
            // A persona that exposes no NetworkInformation at all (a capture whose browser hides
            // `navigator.connection`) still sits on a network: the kernel's declared default
            // connection stands in for it, rather than Blink's "unknown" row, which no connected
            // browser reports once its network-quality estimator has run.
            .unwrap_or_else(|| spec::Connection::default().effective_type);
        // NOTHING MOVED SINCE THE LAST CHECK: its answer is a function of the deferred set (`gen`), the page's
        // documents, boxes, computed overflow and viewport (the layout's key, `Dom::instance`), every scroll offset
        // and the margin row -- and that answer was "nothing due", or `gen` would have moved. Checks re-armed by
        // DOM churn that moved no box cost this hash instead of a look at every deferred element.
        let key = {
            use std::hash::{Hash, Hasher};
            let mut k = std::collections::hash_map::DefaultHasher::new();
            st.try_borrow::<HostDom>().map(|h| h.0.instance()).hash(&mut k);
            st.try_borrow::<crate::core::dom::LayoutCache>().and_then(|c| c.0.as_ref().map(|(fp, _)| *fp)).hash(&mut k);
            let (sx, sy) = crate::core::html::window_geometry::scroll_position(&st);
            (sx.to_bits(), sy.to_bits()).hash(&mut k);
            // order-independent: the scroll store is a map
            let scrolled = crate::core::dom::element::scrolled_elements(&st).into_iter().fold(0u64, |acc, (id, (x, y))| {
                let mut e = std::collections::hash_map::DefaultHasher::new();
                (id, x.to_bits(), y.to_bits()).hash(&mut e);
                acc.wrapping_add(e.finish())
            });
            scrolled.hash(&mut k);
            with(&mut st, |l| l.gen).hash(&mut k);
            effective.hash(&mut k);
            k.finish()
        };
        if with(&mut st, |l| std::mem::replace(&mut l.last_key, Some(key)) == Some(key)) {
            return;
        }
        let margins = margins(&effective);
        // The window's viewport in client coordinates: the same for every element of the check (no script runs).
        let viewport = {
            let media = crate::core::dom::geometry_queries::media_context(&st);
            Box2 { l: 0.0, t: 0.0, r: media.viewport_width, b: media.viewport_height }
        };
        let mut cache = HashMap::new();
        let mut connected = Vec::new();
        for (id, kind) in pending {
            if st.try_borrow::<HostDom>().is_some_and(|h| h.0.is_connected(id)) {
                let s = scrollers(&mut st, id, &mut cache);
                connected.push((id, kind, s));
            }
        }
        (connected, margins, viewport)
    };
    let (connected, (img, frame), viewport) = due;
    let mut clips = HashMap::new();
    let mut due = Vec::new();
    for (id, kind, scrollers) in connected {
        let hit = match kind {
            Kind::Image => near(scope, id, &scrollers, img, img / 2.0, viewport, &mut clips),
            Kind::Frame => near(scope, id, &scrollers, frame, frame, viewport, &mut clips),
        };
        if hit {
            due.push((id, kind));
        }
    }
    with_scope(scope, |l| {
        l.pending.retain(|(p, _)| !due.iter().any(|(d, _)| d == p));
        if !due.is_empty() {
            l.gen += 1;
        }
    });
    for (id, kind) in due {
        match kind {
            Kind::Image => crate::core::html::subresource::image_update(scope, id, true),
            Kind::Frame => crate::core::html::subresource::maybe_load(scope, id, "src", true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_margin_row_follows_the_connection_type() {
        assert_eq!(margins("4g"), (1250.0, 2500.0));
        assert_eq!(margins(""), (3000.0, 4000.0));
    }
}
