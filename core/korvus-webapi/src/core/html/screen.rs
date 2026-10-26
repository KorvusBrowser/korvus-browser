//! `Screen` (drafts.csswg.org/cssom-view/#the-screen-interface), an OWN class.
//!
//! It was `korvus-runtime`'s `#[op2]` class `JsScreen` -- Web API in the kernel (invariant I9) --
//! published on the global as `Screen` by the bootstrap, renamed, spliced onto `EventTarget`, and given
//! `onchange` / `isExtended` by the registry's backfill. Moved here with its getters verbatim (they read
//! the persona through `binding::protos::persona_handle`, the same `Arc` the runtime's persona state
//! held), as a `#[webapi(own)]` class whose table below is the whole interface: a non-newable table
//! interface on `EventTarget`, built in the window's class slot (`install::live::bootstrap`).

use deno_core::{v8, OpState};
use macros::webapi;

/// Native `screen` — stateless shell reading the persona's [`spec::ScreenProfile`] live.
///
/// THE GETTER DECLARATION ORDER BELOW IS `Screen.prototype`'s ENUMERATION ORDER, and it is a
/// fingerprint surface rather than a matter of taste: `deno_core` realises an `#[op2] impl`'s class
/// template in source order, and `Object.getOwnPropertyNames(Screen.prototype)` is three lines for a
/// page to read. Measured on `google-chrome --headless=new --dump-dom`, 2026-09-12, and identical in
/// the shipped capture (`install::generated_registry`'s `Screen` rows):
///
/// ```text
///   availWidth availHeight width height colorDepth pixelDepth availLeft availTop
///   orientation constructor onchange isExtended
/// ```
///
/// It used to be declared `width height colorDepth pixelDepth availWidth availHeight availTop
/// availLeft`, and a whole second generated table (`install/generated_order.rs`, 407 interfaces) plus
/// a post-hoc delete-and-redefine pass existed to shuffle it back afterwards. `Screen` was the ONLY
/// interface that pass ever moved — measured at 773 realm assemblies across the runtime suite
/// — so fixing the order at its one writer deleted both.
#[repr(C)]
pub struct JsScreen;

/// The worn persona's screen (the same `Arc<dyn Persona>` the runtime's host persona state held), or
/// the empty profile in a realm with no persona service.
fn screen(state: &OpState) -> spec::ScreenProfile {
    spec::devtools::screen(crate::binding::protos::persona_handle(state).map(|p| p.0.screen()).unwrap_or_default())
}

#[webapi(own, name = "Screen")]
impl JsScreen {

    #[getter]
    fn avail_width(&self, state: &OpState) -> u32 {
        let s = screen(state);
        if s.avail_width > 0 { s.avail_width } else { s.width }
    }

    #[getter]
    fn avail_height(&self, state: &OpState) -> u32 {
        let s = screen(state);
        if s.avail_height > 0 { s.avail_height } else { s.height }
    }

    #[getter]
    fn width(&self, state: &OpState) -> u32 {
        screen(state).width
    }

    #[getter]
    fn height(&self, state: &OpState) -> u32 {
        screen(state).height
    }

    #[getter]
    fn color_depth(&self, state: &OpState) -> u32 {
        screen(state).color_depth
    }

    #[getter]
    fn pixel_depth(&self, state: &OpState) -> u32 {
        screen(state).color_depth
    }

    #[getter]
    fn avail_left(&self, state: &OpState) -> i32 {
        screen(state).avail_left
    }
    #[getter]
    fn avail_top(&self, state: &OpState) -> i32 {
        screen(state).avail_top
    }

    /// `screen.orientation`.
    ///
    /// DECLARED HERE, LAST, AND THAT IS THE MEMBER'S SEAT. The captured `Screen.prototype` puts
    /// `orientation` at index 8 and `constructor` at 9, with the partial-interface members
    /// (`onchange`, `isExtended`) after it; a `deno_core` class template realises its accessors
    /// BEFORE V8 puts `constructor` on the prototype, so a getter declared after `availTop` lands at
    /// 8 and `constructor` moves to 9. Measured on real Brave 151.0.7922.173 over CDP (2026-09-19):
    /// `Object.getOwnPropertyNames(Screen.prototype)` = `availWidth availHeight width height
    /// colorDepth pixelDepth availLeft availTop orientation constructor`.
    ///
    /// The registry's `Screen::orientation` row cannot seat it: the backfill only ADDS what the
    /// prototype does not already own, so a row installed there lands AFTER `constructor` -- which
    /// is where korvus answered `undefined` from until this lane.
    ///
    /// The OBJECT is `webapi::screen_orientation`'s, built from the same persona every getter above
    /// reads; the kernel defines no Web API of its own (invariant I9).
    #[getter]
    fn orientation<'s>(&self, scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
        crate::core::html::screen_orientation::screen_orientation(scope)
    }

    /// `screen.isExtended` (Window Management) -- `false`: a persona describes one display. It was the
    /// registry's D2 stub, whose shape-correct answer is the same `false`; as the class's own getter it
    /// also refuses a receiver that is not a `Screen` wrapper (`Object.create(Screen.prototype)`), as
    /// Chrome does -- the table's stub row accepted one.
    #[getter]
    fn is_extended(&self) -> bool {
        false
    }
}

webapi::interface! {
    "Screen" : "EventTarget" [interface, window, class JsScreen] {
        ctor    "Screen"                              len 0 = stub,
        attr    "availWidth"  : Integer readonly            = cppgc,
        attr    "availHeight" : Integer readonly            = cppgc,
        attr    "width"       : Integer readonly            = cppgc,
        attr    "height"      : Integer readonly            = cppgc,
        attr    "colorDepth"  : Integer readonly            = cppgc,
        attr    "pixelDepth"  : Integer readonly            = cppgc,
        attr    "availLeft"   : Any     readonly            = cppgc,
        attr    "availTop"    : Any     readonly            = cppgc,
        attr    "orientation" : Any     readonly            = cppgc,
        handler "onchange"    nullable                      = event_handler,
        attr    "isExtended"  : Boolean readonly            = cppgc,
    }
}
