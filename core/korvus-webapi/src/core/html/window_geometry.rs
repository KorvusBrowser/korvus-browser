//! `core/html/window_geometry.rs` -- the `Window`'s thirteen geometry attributes and its three scroll
//! methods.
//!
//! WHAT THIS REPLACES: the last big block of `core/korvus-runtime/src/js/host.js` -- `innerWidth`,
//! `innerHeight`, `outerWidth`, `outerHeight`, `devicePixelRatio`, `screenX`/`screenLeft`,
//! `screenY`/`screenTop`, `scrollX`/`pageXOffset`, `scrollY`/`pageYOffset`, and
//! `scrollTo`/`scroll`/`scrollBy`. Three `Object.defineProperty(globalThis, …)` helpers covered all
//! sixteen members (`dim`, `replaceable`, `meth`), behind three `op_nativize` mask sites -- invariants
//! I2 and I3, in the kernel (I9). The freeze counts what is spelled: `define_property_js` 6 -> 3,
//! `native_masks` 24 -> 18. They are rows of the
//! `Window` table now, mounted by the one installer onto the global -- which is where a `[Global]`
//! interface's members live, and where the JS put them.
//!
//! # What the rows fix, measured on `google-chrome --headless=new` 150.0.7871.46
//!
//! The SHAPE the JS had was already right and is kept exactly: every one is a `[Replaceable]`
//! accessor pair -- `e=1 c=1`, `get X` of length 0, `set X` of length 1 -- whose setter deletes the
//! accessor and leaves an own data property (`w=1 e=1 c=1`), so `window.innerWidth = 5` sticks.
//! Chrome agrees on all thirteen, on `window` itself and not on `Window.prototype`.
//!
//! What it did NOT have is the RECEIVER CHECK. `Object.getOwnPropertyDescriptor(window,
//! "innerWidth").get.call({})` throws `TypeError: Illegal invocation` in Chrome, and so does every
//! one of the thirteen setters; the JS bodies ignored `this` entirely and answered. That is the same
//! finding `window.rs`'s `recv` records for the other 157 rows, re-measured for these -- and it is
//! why they go through `guarded` like every other row of the table.
//!
//! `scrollTo` / `scroll` / `scrollBy` are NOT guarded, and that is measured too:
//! `window.scrollTo.call({})` does NOT throw in Chrome. They are the only three members of this group
//! that accept a foreign receiver.
//!
//! IT DOES NOT THROW; IT REJECTS, and that is a different sentence. Measured 2026-09-23 on Chrome
//! 150.0.7871.46 (case C9):
//! `window.scrollTo.call({}, 0, 0)` answers an ALREADY-REJECTED promise carrying
//! `TypeError: Failed to execute 'scrollTo' on 'Window': Illegal invocation` -- which is exactly
//! what `tests/fixtures/p12g-frame-sweep.chrome-150.json` recorded independently as
//! `"foreign": "rej:..."` for all three names. That is the shape `window_globals.rs` already
//! describes for a promise-returning global body ("rejects with its own sentence instead" of
//! throwing), and this file does NOT implement it yet: these rows answer a RESOLVED promise for
//! every receiver. It is why `scroll`/`scrollTo`/`scrollBy` are still in
//! `tests/frame_globals_live.rs`'s `NOT_ASSERTED`.
//!
//! HOW MUCH IS LEFT IS MEASURED, NOT ESTIMATED. Dropping those three names from `NOT_ASSERTED` and
//! running the sweep on this commit fails with exactly THREE cells of eighteen, in both the `src`
//! and the `srcdoc` frame:
//!   `scroll|scrollTo|scrollBy foreign: korvus "res:Object" chrome "rej:...Illegal invocation"`.
//! The other fifteen -- `direct`, `bare`, `self`, `parent`, `pageFnFrameRecv` for all three names --
//! now agree with Chrome. So the grandfather is down to one cell per name and one mechanism.
//!
//! # The fallback chain has ONE definition now
//!
//! [`window_geometry`] was `op_window_geometry` in the kernel (`korvus-runtime/src/lib.rs`), reading
//! `HostPersona` out of `OpState`. It reads the SAME persona through `spec::PersonaHandle` on the
//! kernel `Context` here -- the route `performance.memory` took in W4.d and `Navigator` in W4.a -- so
//! the op is deleted and the kernel defines no Web API (I9).
//!
//! That move also closes a DUPLICATE this file's absence forced:
//! `core/dom/geometry_queries.rs::media_context` carried its own copy of the inner-size fallback
//! chain, with a comment saying so ("the fallback chain is the profile's, and lives once in the
//! engine's window_geometry op; mirrored here because the cascade cannot call an op"). It calls this
//! function now. The two could disagree; they cannot any more, which is the whole point --
//! `@media (height: 982px)` and `window.innerHeight` are the same number by construction.

use deno_core::{v8, OpState};

use crate::binding::protos::build_fn_data;

/// The window's geometry, as six numbers: `[innerW, innerH, outerW, outerH, screenX, screenY]`.
///
/// One function rather than six, so the FALLBACK CHAIN lives in one place -- getting it wrong is a
/// coherence tell rather than a missing feature. Every value is the profile's; nothing here is a
/// constant.
///
///   outer  <- captured window outer, else the desktop WORK AREA (a maximized window), else screen
///   inner  <- captured window inner, else outer
///   screen <- captured window position, else the work area's origin (`availLeft`/`availTop`)
///
/// A realm with no persona service answers all zeroes, which is what the op did when `HostPersona`
/// was absent -- it panicked, in fact; this is the same surface without that edge.
pub(crate) fn window_geometry(state: &OpState) -> [f64; 6] {
    let Some(persona) = crate::binding::protos::persona_handle(&state) else {
        return [0.0; 6];
    };
    let mut g = window_geometry_of(&persona.0.screen());
    // A remote frame's window is its `<iframe>`'s content box; outer size and screen position stay the
    // top window's, as a frame's do in Chrome.
    if let Some((w, h)) = crate::core::html::remote_frame::embedded_viewport(state) {
        g[0] = w;
        g[1] = h;
    }
    g
}

/// [`window_geometry`] over a profile the caller already holds.
///
/// PUBLIC, and for one caller outside this crate: `korvus-runtime`'s `input.rs::viewport`, which
/// turns a synthetic click into viewport coordinates and whose doc comment already said it wanted
/// "the same numbers `window.innerWidth` answers, from the same `window_geometry`". It held a
/// `&ScreenProfile` rather than an `OpState`, so when the chain lived in the kernel it could call it
/// directly; this split keeps that true across the crate boundary instead of giving the driver a
/// third copy of the chain.
pub fn window_geometry_of(s: &spec::ScreenProfile) -> [f64; 6] {
    let g = profile_window(s);
    let g = match *window_bounds().read().unwrap_or_else(|e| e.into_inner()) {
        Some(b) => resized(s, g, b),
        None => g,
    };
    // A DevTools client's viewport (`Emulation.setDeviceMetricsOverride`) is the layout viewport, as Chrome's
    // emulation makes it: the inner size is the client's, the window's own chrome kept around it.
    match spec::devtools::viewport() {
        Some((w, h)) => {
            let (chrome_w, chrome_h) = ((g[2] - g[0]).max(0.0), (g[3] - g[1]).max(0.0));
            [w, h, w + chrome_w, h + chrome_h, g[4], g[5]]
        }
        None => g,
    }
}

/// The window as a front end left it: `Browser.setWindowBounds` on this page's window (D75), which the router hands
/// this page's process. Process-wide, as the window is: one page per process. `None` until a front end resizes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowBounds {
    pub left: Option<f64>,
    pub top: Option<f64>,
    /// Outer size, the window's own frame included, as `Browser.Bounds` gives it.
    pub width: Option<f64>,
    pub height: Option<f64>,
    /// `normal`, `maximized` (the work area), `fullscreen` (the screen, no browser chrome); `minimized` keeps the size.
    pub state: WindowState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowState {
    Normal,
    Minimized,
    Maximized,
    Fullscreen,
}

fn window_bounds() -> &'static std::sync::RwLock<Option<WindowBounds>> {
    static B: std::sync::OnceLock<std::sync::RwLock<Option<WindowBounds>>> = std::sync::OnceLock::new();
    B.get_or_init(Default::default)
}

/// Set the window's bounds (D75). A bound left out keeps what the window had; `[innerW, innerH]` before and after, so
/// the caller knows whether the viewport moved (and owes the page a `resize`).
pub fn set_window_bounds(s: &spec::ScreenProfile, b: WindowBounds) -> ([f64; 2], [f64; 2]) {
    let before = window_geometry_of(s);
    {
        let mut cur = window_bounds().write().unwrap_or_else(|e| e.into_inner());
        let prev = cur.unwrap_or(WindowBounds { left: None, top: None, width: None, height: None, state: WindowState::Normal });
        *cur = Some(WindowBounds {
            left: b.left.or(prev.left),
            top: b.top.or(prev.top),
            width: b.width.or(prev.width),
            height: b.height.or(prev.height),
            state: b.state,
        });
    }
    let after = window_geometry_of(s);
    ([before[0], before[1]], [after[0], after[1]])
}

/// The profile's window ([`window_geometry_of`] before any resize) moved and sized as `b` says, kept on the persona's
/// screen: the outer size within the work area (`maximized` fills it, `fullscreen` fills the screen and drops the
/// browser's chrome), the position so the window stays on it, and the inner size the outer less the profile's own
/// chrome -- so `outerHeight - innerHeight` stays the captured browser's tab strip and toolbar.
fn resized(s: &spec::ScreenProfile, g: [f64; 6], b: WindowBounds) -> [f64; 6] {
    let [iw, ih, ow, oh, sx, sy] = g;
    let (chrome_w, chrome_h) = ((ow - iw).max(0.0), (oh - ih).max(0.0));
    let (area_w, area_h) = (
        if s.avail_width > 0 { s.avail_width as f64 } else { ow },
        if s.avail_height > 0 { s.avail_height as f64 } else { oh },
    );
    let (area_x, area_y) = (s.avail_left as f64, s.avail_top as f64);
    match b.state {
        WindowState::Fullscreen => {
            let (w, h) = (if s.width > 0 { s.width as f64 } else { area_w }, if s.height > 0 { s.height as f64 } else { area_h });
            [w, h, w, h, 0.0, 0.0]
        }
        WindowState::Maximized => [(area_w - chrome_w).max(1.0), (area_h - chrome_h).max(1.0), area_w, area_h, area_x, area_y],
        WindowState::Normal | WindowState::Minimized => {
            let w = b.width.unwrap_or(ow).round().clamp(chrome_w + 1.0, area_w.max(chrome_w + 1.0));
            let h = b.height.unwrap_or(oh).round().clamp(chrome_h + 1.0, area_h.max(chrome_h + 1.0));
            let x = b.left.unwrap_or(sx).round().clamp(area_x, (area_x + area_w - w).max(area_x));
            let y = b.top.unwrap_or(sy).round().clamp(area_y, (area_y + area_h - h).max(area_y));
            [w - chrome_w, h - chrome_h, w, h, x, y]
        }
    }
}

/// The window the persona's profile describes: [`window_geometry_of`]'s fallback chain, before any resize.
fn profile_window(s: &spec::ScreenProfile) -> [f64; 6] {
    let pick = |captured: u32, avail: u32, full: u32| -> f64 {
        if captured > 0 {
            captured as f64
        } else if avail > 0 {
            avail as f64
        } else {
            full as f64
        }
    };
    let outer_w = pick(s.window_outer_width, s.avail_width, s.width);
    let outer_h = pick(s.window_outer_height, s.avail_height, s.height);
    let inner_w = if s.window_inner_width > 0 { s.window_inner_width as f64 } else { outer_w };
    let inner_h = if s.window_inner_height > 0 { s.window_inner_height as f64 } else { outer_h };
    let sx = if s.window_screen_x != 0 { s.window_screen_x } else { s.avail_left };
    let sy = if s.window_screen_y != 0 { s.window_screen_y } else { s.avail_top };
    [inner_w, inner_h, outer_w, outer_h, sx as f64, sy as f64]
}

/// The window's viewport changed size (D75): `resize` at the window, as a task -- HTML's "run the resize steps", which
/// fire one trusted, non-bubbling, non-cancelable `resize` when the viewport's size changed since the last frame.
pub fn window_resized(scope: &mut v8::PinScope) {
    let cb = crate::binding::protos::build_fn(scope, cb_fire_resize, "", 0);
    crate::core::intersection_observer::rendering::engine_task(scope, cb, 0.0);
}

fn cb_fire_resize(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(evc) = crate::binding::protos::iface_object(scope, "Event").and_then(|c| v8::Local::<v8::Function>::try_from(c).ok()) else { return };
    let ty = crate::binding::protos::str_val(scope, "resize");
    let Some(ev) = evc.new_instance(scope, &[ty]) else { return };
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    let ctx = scope.get_current_context();
    let win: v8::Local<v8::Value> = ctx.global(scope).into();
    crate::binding::protos::engine_call(scope, win, "dispatchEvent", &[ev.into()]);
}

/// `window.devicePixelRatio` -- the persona screen's, read at CALL time like every other row.
fn device_pixel_ratio(state: &OpState) -> f64 {
    crate::binding::protos::persona_handle(&state)
        .map(|p| spec::devtools::screen(p.0.screen()).device_pixel_ratio)
        .unwrap_or(0.0)
}

/// The realm's scroll position -- which is the DOCUMENT ELEMENT's, and the only copy of it.
///
/// There is no compositor, so nothing scrolls on its own; `scrollTo`/`scrollBy` and
/// `documentElement.scrollTop`/`scrollLeft` all write this cell and `scrollX`/`scrollY` read it
/// back. It used to be one of two: the element members kept their own numbers in
/// `element.rs`'s `ScrollState`, so `scrollTo(0, 2e6)` moved `scrollY` and left
/// `documentElement.scrollTop` at 0 while `documentElement.scrollTop = 2e6` did the reverse. Chrome
/// reports one number through both (measured, 150.0.7871.46, 2026-09-13), so there is one here.
///
/// Clamped at 0 -- a browser has no negative scroll offset -- and at the
/// document's own scrollable extent at the far end too.
#[derive(Default)]
pub(crate) struct ScrollPosition {
    pub x: f64,
    pub y: f64,
}

pub(crate) fn scroll_position(state: &OpState) -> (f64, f64) {
    state.try_borrow::<ScrollPosition>().map(|s| (s.x, s.y)).unwrap_or((0.0, 0.0))
}

/// Which number a geometry row reads, bound as the getter's `data` so one callback serves all nine.
///
/// The pairs are Chrome's aliases: `screenLeft` IS `screenX` and `pageXOffset` IS `scrollX`, same
/// value from the same source, which is why they are indices into one list rather than nine bodies.
const GEOM_INDEX: [(&str, usize); 13] = [
    ("innerWidth", 0),
    ("innerHeight", 1),
    ("outerWidth", 2),
    ("outerHeight", 3),
    ("screenX", 4),
    ("screenLeft", 4),
    ("screenY", 5),
    ("screenTop", 5),
    // 6 = devicePixelRatio, 7 = scrollX, 8 = scrollY: not indices into the geometry array, but the
    // same one-callback dispatch.
    ("devicePixelRatio", 6),
    ("scrollX", 7),
    ("pageXOffset", 7),
    ("scrollY", 8),
    ("pageYOffset", 8),
];

/// The frame element and document element of the nested browsing context whose WINDOW `this` is --
/// `None` for the page's own window and for anything else.
fn frame_window(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<(u32, Option<u32>)> {
    let ctx = crate::core::html::browsing_context::ctx_of_window(scope, this)?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let (el, root) = crate::core::html::browsing_context::frame_parts(&st, ctx)?;
    let de = st.try_borrow::<crate::binding::host_dom::HostDom>().and_then(|d| crate::core::dom::document::doc_element_child(&d.0, root));
    Some((el, de))
}

/// The viewport of browsing context `ctx`'s window -- its frame element's content box, what the frame's
/// `innerWidth`/`innerHeight` answer. `None` for a context with no frame element (a popup).
pub(crate) fn frame_viewport_size(scope: &mut v8::PinScope, ctx: u32) -> Option<(f64, f64)> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let el = {
        let st = op_state.borrow();
        crate::core::html::browsing_context::frame_parts(&st, ctx)?.0
    };
    let mut st = op_state.borrow_mut();
    Some(crate::core::dom::geometry_queries::element_content_size(&mut st, el))
}

/// A frame window's `innerWidth` / `innerHeight` (`axis` 0 / 1), `scrollX` / `scrollY` (7 / 8).
///
/// Its viewport is the frame element's content box, and its scroll is its own document element's --
/// measured on Chrome 150.0.7871.46 over loopback: a `300x200` iframe answers `innerWidth` 300 and
/// `innerHeight` 200 (the page's window is 1920 wide), a `250x150` one 250x150, and after
/// `frame.contentWindow.scrollTo(0, 70)` the frame reads `scrollY` 70 while the page stays at 0. The
/// other rows (`outerWidth`, `screenX`, `devicePixelRatio`) answer for the top-level window in a
/// frame too, so they keep the page's numbers.
fn frame_geometry(scope: &mut v8::PinScope, el: u32, de: Option<u32>, index: i64) -> Option<f64> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    match index {
        0 | 1 => {
            let mut st = op_state.borrow_mut();
            let size = crate::core::dom::geometry_queries::element_content_size(&mut st, el);
            Some(if index == 0 { size.0 } else { size.1 })
        }
        7 | 8 => {
            let st = op_state.borrow();
            let (x, y) = de.map_or((0.0, 0.0), |de| crate::core::dom::element::element_scroll_offset(&st, de));
            Some(if index == 7 { x } else { y })
        }
        _ => None,
    }
}

fn cb_geometry_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(i) = crate::binding::protos::body_data(scope, &args).integer_value(scope) else { return };
    if let Some((el, de)) = frame_window(scope, args.this()) {
        if let Some(v) = frame_geometry(scope, el, de, i) {
            rv.set_double(v);
            return;
        }
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let v = match i {
        6 => device_pixel_ratio(&st),
        7 => scroll_position(&st).0,
        8 => scroll_position(&st).1,
        n => window_geometry(&st)[n as usize],
    };
    drop(st);
    rv.set_double(v);
}

/// `scrollTo(x, y)` / `scrollTo(options)` and `scroll`, which is its alias; `scrollBy` when `by`.
///
/// `(double x, double y)` or a `ScrollToOptions` dictionary -- every argument of every overload is
/// optional, which is why the WebIDL `length` is 0 and a bare `scrollTo()` is legal.
///
/// THE DESTINATION IS THE DOCUMENT'S OWN SCROLLABLE EXTENT, and it is not computed here. This body
/// reads the arguments and hands them to [`crate::core::dom::element::scroll_document`], which is
/// also where `documentElement.scrollTop` goes: one clamp, one store, one scroll event, because the
/// two members are one operation.
///
/// What that clamp replaced is the whole of the `scroll` cluster. `pos.y = y.max(0.0)` floored
/// the position at 0 and had no ceiling, "because nothing in this file knows the document's extent"
/// -- so `window.scrollTo(0, 2e6)` left `scrollY` reading 2000000 on 797 corpus pages. That is an
/// assignment, not a scroll, and the interactivity axis's `scroll` capability was written as "moved
/// AND landed at its own `scrollHeight - innerHeight`" so that the difference would be visible.
fn cb_scroll(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // the data is the row's own name: `scrollBy` moves by, and a refusal names the method it was called as
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let by = name == "scrollBy";
    // THE ANSWER IS A PROMISE, and it is the SAME promise `Element.prototype.scrollTo` answers --
    // `crate::core::dom::element::scroll_promise`, which is where what it resolves to and why is
    // written down. This body took `_rv` and set nothing, so all six rows here (three names, both
    // arities) answered `undefined` where the capture records `[object Promise]`.
    //
    // The ARGUMENT parser below is shared with the element rows; the RETURN VALUE was not, and that
    // is the whole of this defect: the element callbacks in `core/dom/element.rs` had been given a
    // promise and these three had not. Set it FIRST, so that every `return` path below -- the frame
    // window's included -- answers it.
    rv.set(crate::core::dom::element::scroll_promise(scope));
    // ONE argument parser for both realms: `window.scrollTo` and `Element.prototype.scrollTo` take
    // their arguments identically in Chrome (measured over the eighteen cases tabulated at
    // `element::scroll_args`), so reading them twice could only ever produce two answers.
    let (x, y) = match crate::core::dom::element::scroll_args(scope, &args, &format!("Failed to execute '{name}' on 'Window': ")) {
        Ok(xy) => xy,
        Err(e) => {
            let p = crate::binding::protos::rejected_promise(scope, e);
            rv.set(p);
            return;
        }
    };
    // A frame's window scrolls the frame's document, not the page.
    if let Some((_, de)) = frame_window(scope, args.this()) {
        if let Some(de) = de {
            crate::core::dom::element::scroll_frame_document(scope, de, x, y, by);
        }
        return;
    }
    // `scrollBy` always moves by both deltas, treating an axis its arguments did not name as a 0
    // delta; `scrollTo` leaves such an axis where it is. Both distinctions live in
    // `scroll_document`, and which axes were named is `scroll_args`'s answer -- a positional `null`
    // NAMES its axis and means the number 0, which is the one `window_dims.rs` had pinned the other
    // way.
    crate::core::dom::element::scroll_document(scope, x, y, by);
}

/// Is `name` one of this file's rows?
pub(crate) fn owns(name: &str) -> bool {
    GEOM_INDEX.iter().any(|(n, _)| *n == name) || matches!(name, "scrollTo" | "scroll" | "scrollBy")
}

/// The getter for a geometry row, and the `[Replaceable]` setter is `window.rs`'s -- the same one
/// `length` and `event` use, because it is the same WebIDL extended attribute.
pub(crate) fn getter<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> Option<v8::Local<'s, v8::Function>> {
    let (_, i) = GEOM_INDEX.iter().find(|(n, _)| *n == name)?;
    let d = crate::binding::protos::int_val(scope, *i as i64);
    Some(build_fn_data(scope, cb_geometry_get, &format!("get {name}"), 0, d))
}

/// The body of a `scrollTo` / `scroll` / `scrollBy` row.
pub(crate) fn scroll_method<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Function> {
    let d = crate::binding::protos::str_val(scope, name);
    build_fn_data(scope, cb_scroll, name, 0, d)
}

#[cfg(test)]
mod tests {
    use super::window_geometry_of;

    /// Every bundle shipped in `plugins/korvus-persona/bundles`, by capture id.
    ///
    /// Read from the DIRECTORY rather than listed here: a bundle added without a captured window is
    /// exactly the case these three relations exist to catch, and a hard-coded list would let it in
    /// unmeasured.
    fn bundle_ids() -> Vec<String> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugins/korvus-persona/bundles");
        let mut ids: Vec<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        ids.sort();
        assert!(!ids.is_empty(), "no persona bundle in the tree");
        ids
    }

    /// R20: a HEADLESS engine wearing a HEADED capture answers the captured window, and the three
    /// relations that follow from one hold in every bundle.
    ///
    /// WHY THESE THREE. They are the cheapest headless tells there are, and each is one subtraction:
    ///
    ///   1. `outerHeight - innerHeight` is the browser's own chrome. Headless Chrome answers 0 --
    ///      no tab strip, no toolbar -- and so does any engine whose fallback chain sets
    ///      `inner = outer` because the capture carried no window. Measured on the four captures the
    ///      difference is 115 / 87 / 84 / 114 px (brave / chrome / edge / opera); brave's and opera's
    ///      carry the profile's bookmarks bar, chrome's 87 is its bare chrome (FP8).
    ///   2. `screenY == availTop` -- a maximized window starts at the top of the work area, below the
    ///      panel. A window at `screenY = 0` claims to overlap the shell's own bar.
    ///   3. `screenY + outerHeight <= screen.height` -- a window that does not fit on the screen it
    ///      claims to be on is not a window anyone has.
    ///
    /// This asserts them on `window_geometry_of`, the ONE fallback chain (`innerHeight` and
    /// `@media (height: …)` both read it), not on the JSON -- a bundle can be right and the chain
    /// still answer something else, which is the failure that matters.
    #[test]
    fn every_bundle_answers_a_headed_window_through_the_geometry_chain() {
        for id in bundle_ids() {
            let p = persona::CapturedPersona::select(Some(&id)).unwrap_or_else(|e| panic!("{id}: {e}"));
            let s = spec::Persona::screen(&p);
            let [_iw, ih, _ow, oh, _sx, sy] = window_geometry_of(&s);

            let chrome = oh - ih;
            assert!(
                chrome > 0.0,
                "{id}: outerHeight {oh} - innerHeight {ih} = {chrome} -- zero chrome is the headless \
                 tell; the capture carries no window, or the fallback chain set inner = outer"
            );
            assert!(
                chrome < 400.0,
                "{id}: {chrome} px of browser chrome is not a window anyone has"
            );
            assert_eq!(
                sy, s.avail_top as f64,
                "{id}: screenY {sy} != availTop {} -- a maximized window starts at the work area's top",
                s.avail_top
            );
            assert!(
                sy + oh <= s.height as f64,
                "{id}: screenY {sy} + outerHeight {oh} = {} exceeds screen.height {} -- the window \
                 does not fit on the screen it claims",
                sy + oh,
                s.height
            );
        }
    }

    /// The detector itself: a profile with NO captured window answers zero chrome.
    ///
    /// This is not a wish -- it is what `window_geometry_of` does today, on purpose, because a realm
    /// with no persona must still answer numbers. The test pins it so the shape is known and so the
    /// test above is the thing that catches a bundle falling into this path, rather than the engine
    /// quietly shipping a headless window.
    #[test]
    fn a_profile_with_no_captured_window_answers_zero_chrome_and_is_caught_above() {
        let s = spec::ScreenProfile {
            width: 1920,
            height: 1080,
            avail_width: 1920,
            avail_height: 1052,
            avail_top: 28,
            avail_left: 0,
            ..Default::default()
        };
        let [iw, ih, ow, oh, _sx, sy] = window_geometry_of(&s);
        assert_eq!((iw, ow), (1920.0, 1920.0));
        assert_eq!((ih, oh), (1052.0, 1052.0), "no captured window -> inner = outer");
        assert_eq!(oh - ih, 0.0, "which is zero chrome -- the tell");
        assert_eq!(sy, 28.0, "the work area's origin still stands in for the position");
    }
}
