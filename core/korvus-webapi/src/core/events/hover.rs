//! `core/events/hover.rs` — moving the pointer: the hover chain, and the four events that report it.
//!
//! # What this is
//!
//! `Dom::hover_chain` is the state `:hover` matches and
//! `LaidOut::hover_chain_at` turns a viewport point into one (P2b.2). This is the piece that runs
//! when the pointer actually moves: hit-test, commit the new chain, and dispatch
//! `mouseout` / `mouseleave` / `mouseover` / `mouseenter` the way a browser does.
//!
//! The state is committed BEFORE the first event is dispatched, and that is measured rather than
//! assumed. Driving stock Chrome 150.0.7871.46 over CDP with a listener that snapshots
//! `querySelectorAll(":hover")` at the moment it runs: inside `mouseout` on the element being
//! LEFT, the snapshot already reported the NEW chain. A handler that reads `getComputedStyle`
//! from a `mouseout` sees the post-move style, so committing afterwards would be wrong.
//!
//! # The order, asked of the browser rather than read off the spec
//!
//! Stock Chrome 150 headed under Xvfb, launched with `--remote-debugging-port` (never by
//! Playwright's launcher, which adds `--enable-automation`), driven by
//! `Input.dispatchMouseEvent` `mouseMoved` over a nested `div#outer > div#mid > div#inner`, with a
//! listener on `window`, `document` and every element for all five mouse events. Writing `o` for
//! the innermost element of the OLD chain and `n` for the innermost of the NEW one:
//!
//! ```text
//! 1. mouseout    on o          bubbles       relatedTarget = n
//! 2. mouseleave  on each element of OLD \ NEW, INNERMOST → OUTERMOST, no bubble, related = n
//! 3. mouseover   on n          bubbles       relatedTarget = o
//! 4. mouseenter  on each element of NEW \ OLD, OUTERMOST → INNERMOST, no bubble, related = o
//! 5. mousemove   on n          bubbles                        (the caller's, not ours)
//! ```
//!
//! Every leave fires before the first over — the two families do not interleave. Verbatim, from
//! the run that settled it (pointer leaving `#leaf` deep inside the nest for `#outer`):
//!
//! ```text
//! mouseout    on #leaf    related=#outer
//! mouseleave  on #leaf    related=#outer
//! mouseleave  on #inner   related=#outer
//! mouseleave  on #mid     related=#outer
//! mouseover   on #outer   related=#leaf
//! mousemove   on #outer   related=null
//! ```
//!
//! and, entering the nest from `<body>`, the mirror image:
//!
//! ```text
//! mouseout    on body     related=#inner
//! mouseover   on #inner   related=body
//! mouseenter  on #outer   related=body
//! mouseenter  on #mid     related=body
//! mouseenter  on #inner   related=body
//! mousemove   on #inner   related=null
//! ```
//!
//! A move that does not change the chain fires NOTHING here — Chrome emits the bare `mousemove`
//! and restyles nothing, which is also why `Dom::set_hover_chain` does not bump the revision then.
//!
//! On the very first move of a document's life there is no old chain: Chrome fired no `mouseout`
//! and no `mouseleave`, a `mouseover` with `relatedTarget === null`, and a `mouseenter` on every
//! element of the new chain. That falls out of "old is empty" without a special case.
//!
//! # Where it went (FIXER C6, 2026-10-11)
//!
//! The hit test, the chain commit and the four boundary events moved into `core/events/pointer_input.rs`, which
//! is Blink's whole mouse pipeline: the POINTER family (`pointerout`/`pointerleave`/`pointerover`/`pointerenter`)
//! now fires on the same chain immediately before the mouse family, the boundary events are computed from the
//! element under the mouse as Blink's `BoundaryEventDispatcher` does (the document included -- with real input
//! stock Chrome 150 fired `mouseenter` at a document listening for it), an enter or leave goes only to a node with
//! a listener for it or under a capturing one, and a frame's document is hit-tested too. What stays here is the
//! old entry point.

use deno_core::v8;

/// Move the pointer to `(x, y)` with `buttons` held: one platform move through
/// `pointer_input::mouse_input` -- `:hover`, the boundary events of both families, `pointermove` and
/// `mousemove`. Returns how many events were dispatched.
pub fn move_pointer_to(scope: &mut v8::PinScope, x: f64, y: f64, buttons: i32, timestamp_ms: f64) -> usize {
    let input = crate::core::events::pointer_input::MouseInput::moved(x, y, buttons as u16, timestamp_ms);
    crate::core::events::pointer_input::mouse_input(scope, &input)
}
