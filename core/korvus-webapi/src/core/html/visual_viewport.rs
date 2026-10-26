//! `core/html/visual_viewport.rs` — `VisualViewport` and the `visualViewport` singleton.
//!
//! # The defect this closes, measured before it was fixed
//!
//! `VisualViewport` the INTERFACE was on the global and `window.visualViewport` the INSTANCE was not,
//! which is a shape no browser has. FP13 read it off `zed.dev`:
//!
//! ```text
//! 'visualViewport' in window     chrome 150  true      korvus  false
//! window.visualViewport          chrome 150  [object VisualViewport]
//!                                korvus      ReferenceError -- a BARE reference threw
//! ```
//!
//! A bare `visualViewport` reference is the form a bundler emits, so the name's absence was a load-axis
//! failure and not only a surface one. The registry published the constructor and nothing ever minted
//! the object, because its ten rows were D2 stubs: `width` would have answered `undefined` where the
//! device answers a number, and FP5 correctly refused to install the name that way.
//!
//! # WHAT THE DEVICE ANSWERS — Chrome 150.0.7871.46, Brave 151.1.93.138, Edge 151.0.4129.93 and
//! Opera 135.0.5973.133, headed over CDP under Xvfb, a 1920x1080 window, 2026-09-19
//!
//! ```text
//!                         at the top of the page      after window.scrollTo(0, 300)
//!   offsetLeft/offsetTop  0 / 0                       0 / 0
//!   pageLeft/pageTop      0 / 0                       0 / 300
//!   width/height          1904 / 976                  1904 / 976
//!   scale                 1                           1
//!   for comparison        innerWidth/innerHeight  1919 / 991
//!                         documentElement.clientWidth/clientHeight  1904 / 976
//! ```
//!
//! **`width`/`height` are the DOCUMENT ELEMENT'S CLIENT AREA, not `innerWidth`/`innerHeight`** — 1904
//! beside 1919, fifteen pixels of scrollbar apart on the very page a sensor reads. The other viewport
//! object in this tree disagrees on purpose and says so: `core/html/bar_props.rs`'s
//! `viewport.segments[0]` IS the inner box. Both readings come from one page load of one browser, so
//! "the viewport" is two numbers here and taking either for the other swaps one tell for another.
//!
//! `pageLeft`/`pageTop` track the document's scroll position — 300 after a 300px scroll — and
//! `offsetLeft`/`offsetTop` do not move with it: the offset is the VISUAL viewport's displacement
//! inside the LAYOUT viewport, which only pinch-zoom produces. This engine has no pinch-zoom, `scale`
//! is 1 for the same reason, and 0/0/1 is therefore the device's own answer on an unzoomed page rather
//! than a value this engine invented. (Measured: all four browsers, both origins, 0/0/1 throughout.)
//!
//! # Why a TABLE
//!
//! `tests/install_ledger.rs` refuses a second writer of a slot the installer mounted, so a
//! mint-then-clobber of `VisualViewport.prototype` is not available: FP5 tried it and the ledger
//! caught all 30 slots it touched. A member with a body belongs in the interface's own table, and the
//! table is what takes the interface out of `install/generated_registry.rs` —
//! `tools/sync/table_inventory.py` scans every table header in the tree, so the exclusion is derived.
//!
//! # Exposure
//!
//! **Unconditional**, measured rather than assumed: `visualViewport` is an own
//! property of `window` on `http://web-platform.test:<p>/` — `isSecureContext` false — on all four
//! browsers, exactly as on the loopback page. It is absent from `tools/sync/data/secure_context.json`
//! for that reason, and that file's absence IS the reading: it records every name an insecure origin
//! takes away, over the whole window, on all four.
//!
//! The own descriptor on `window` is `get visualViewport` / `set visualViewport` with e=1 c=1 on all
//! four — a `[Replaceable]` attribute, which is `WindowSetter::Replaceable`.

use deno_core::v8;

use crate::install::installer::{self, Flavor};
use crate::install::MemberDef;
use crate::install::kits::brand_getter;
use crate::install::kits::branded;


/// The scroll position the document is at, which is what `scrollX`/`scrollY` read.
fn document_scroll(scope: &mut v8::PinScope) -> (f64, f64) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    crate::core::html::window_geometry::scroll_position(&st)
}

/// `offsetLeft` / `offsetTop` — the visual viewport's displacement inside the layout viewport, which
/// is 0 on an unzoomed page. `args.data()` carries the prototype (the brand), so the axis is the
/// member name rather than a second `data`.
fn cb_offset(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    }
    rv.set_double(0.0);
}

/// `pageLeft` — the visual viewport's left edge in document coordinates: the document's scroll
/// position plus `offsetLeft`, which is 0 here.
fn cb_page_left(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    }
    let (x, _) = document_scroll(scope);
    rv.set_double(x);
}

/// `pageTop` — see [`cb_page_left`]. 300 after `window.scrollTo(0, 300)` on all four browsers.
fn cb_page_top(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    }
    let (_, y) = document_scroll(scope);
    rv.set_double(y);
}

/// The visual viewport of the realm the getter belongs to: a FRAME's is its own window's (ACCESS
/// 2026-10-04: Chrome 150 answers a 300x65 iframe's `visualViewport` 300x65; korvus answered every frame
/// the page's 1920x965 -- an interactive-widget challenge frame reads it), the page's its document element's
/// client area.
fn realm_viewport(scope: &mut v8::PinScope) -> (f64, f64) {
    if let Some(ctx) = crate::core::html::browsing_context::current_realm_ctx(scope) {
        if let Some(size) = crate::core::html::window_geometry::frame_viewport_size(scope, ctx) {
            return size;
        }
    }
    crate::core::dom::geometry_queries::viewport_client_size(scope)
}

/// `width` — the document element's client width, the SAME layout read
/// `document.documentElement.clientWidth` answers from. Not `innerWidth`: the two differ by the
/// scrollbar on every scrolling page (1904 against 1919, measured).
fn cb_width(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    }
    let (w, _) = realm_viewport(scope);
    rv.set_double(w);
}

/// `height` — see [`cb_width`].
fn cb_height(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    }
    let (_, h) = realm_viewport(scope);
    rv.set_double(h);
}

/// `scale` — the pinch-zoom scale factor, 1 on a page nobody has pinched. This engine has no
/// pinch-zoom input at all, so 1 is its answer for the same reason it is the browser's on the page
/// the rig measured.
fn cb_scale(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if !branded(scope, &args) {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    }
    rv.set_double(1.0);
}

/// Mount one `VisualViewport` attribute row. The three handler rows go through
/// `install::kits::event_handler` from the table and never reach here.
pub fn install_visual_viewport(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "offsetLeft" | "offsetTop" => brand_getter(scope, proto, member, cb_offset),
        "pageLeft" => brand_getter(scope, proto, member, cb_page_left),
        "pageTop" => brand_getter(scope, proto, member, cb_page_top),
        "width" => brand_getter(scope, proto, member, cb_width),
        "height" => brand_getter(scope, proto, member, cb_height),
        "scale" => brand_getter(scope, proto, member, cb_scale),
        other => debug_assert!(false, "install_visual_viewport: unexpected member {other}"),
    }
}

/// The interface from its table, spliced onto `EventTarget`, and the one `visualViewport` singleton.
///
/// `VisualViewport` inherits from `EventTarget` in all four captures and
/// `window.visualViewport instanceof EventTarget` is true on all four browsers, so the instance gets
/// `addEventListener` by inheritance — `events::event_target::et_key_add` allocates a per-object
/// listener list for any non-node receiver, so nothing else has to be plumbed for the two scroll
/// events to be subscribable.
///
/// Position: inside `install_window_extras`, which runs after the registry install (so `EventTarget`
/// exists to splice onto) and before `installer::seat_globals_in_capture_order`, which puts the name
/// at the worn capture's own position.
pub fn install_window_visual_viewport<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::html::exposure::{gate, Exposure, Publishes};
    use crate::core::html::window_globals::{define_window_singleton, mint_shell, Mint, WindowSetter};

    let parent = crate::install::live::rows::parent_of(scope, "EventTarget");
    let mut ledger = installer::take_ledger(scope);
    let built = installer::create_interface(
        scope,
        global,
        &VISUALVIEWPORT,
        VISUALVIEWPORT.name,
        parent,
        None,
        Flavor::Table,
        &mut ledger,
    );
    for g in &built.gaps {
        eprintln!("[korvus] visual-viewport install gap: {}.{}", g.interface, g.member);
    }
    installer::put_ledger(scope, ledger);

    if !gate(scope, Publishes::Global, "visualViewport", Exposure::Unconditional) {
        return;
    }
    let Some(o) = mint_shell(scope, "VisualViewport") else { return };
    define_window_singleton(scope, global, "visualViewport", o.into(), WindowSetter::Replaceable, true, Mint::Shell("VisualViewport"));
}

// The table. All four captures record these ten prototype members, in this order, all enumerable and
// configurable, the seven attributes as getter-only accessors named `get <name>` of length 0 and the
// three handlers as get/set pairs; `ctorIndex` is 10 in all four, which
// the create places from the capture (`Mount::ctor_at`) rather than this table.
// `new VisualViewport()` is `TypeError: Failed to construct 'VisualViewport': Illegal constructor` on
// all four, which `Model::Interface` plus a `stub` ctor row spells.
webapi::interface! {
    "VisualViewport" : "EventTarget" [interface, window] {
    ctor    "VisualViewport"            len 0             = stub,
    attr    "offsetLeft"  : Double readonly               = install_visual_viewport,
    attr    "offsetTop"   : Double readonly               = install_visual_viewport,
    attr    "pageLeft"    : Double readonly               = install_visual_viewport,
    attr    "pageTop"     : Double readonly               = install_visual_viewport,
    attr    "width"       : Double readonly               = install_visual_viewport,
    attr    "height"      : Double readonly               = install_visual_viewport,
    attr    "scale"       : Double readonly               = install_visual_viewport,
    handler "onresize"                                    = event_handler,
    handler "onscroll"                                    = event_handler,
    handler "onscrollend"                                 = event_handler
    }
}
