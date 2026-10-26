//! `core/events/user_activation.rs` -- USER ACTIVATION: the page's record that a person did something.
//!
//! HTML's user-activation model as Blink 150 keeps it (`third_party/blink/public/common/frame/
//! user_activation_state.h`, `.cc`): a STICKY bit that, once set, stays for the life of the document
//! (`navigator.userActivation.hasBeenActive`), and a TRANSIENT one that holds for
//! `kActivationLifespan` -- **5 seconds** on desktop (`user_activation_state.h:23`) -- after the last
//! activating input (`navigator.userActivation.isActive`), and that an API which spends it (a popup, a
//! fullscreen request) CONSUMES early.
//!
//! What activates is the INPUT, not the event a page sees: Blink calls `LocalFrame::NotifyUserActivation`
//! from the input managers BEFORE the DOM event is dispatched, so a `keydown` listener already reads
//! `isActive === true`. For a key it is any `keydown` that is not a modifier key and not Escape
//! (`keyboard_event_manager.cc:249-259`, the `IsDomKeyForModifier` test); for the pointer it is the
//! press -- the mouse half calls [`notify_user_activation`] from its own path.
//!
//! What READS it is everything that behaves differently for a person than for a script. The first
//! reader wired here is the navigation request (`browsing_context::request_top_navigation`): Chrome sends
//! `Sec-Fetch-User: ?1` on a navigation that began inside the transient window of a trusted activation
//! (`services/network/sec_header_helpers.cc:176-182`), and measured (2026-10-11, a production site
//! investigator's `probe/activation`): a trusted link click, or a click whose handler navigates at once,
//! sends `?1`; a navigation 6 s after the click, or a script navigation with no input at all, sends none.
//!
//! A new document starts with neither bit (Blink clears both on a cross-document navigation), which here
//! is free: the next document gets a fresh engine and with it a fresh `OpState` (`Engine::successor`).
//!
//! ONE STATE PER PAGE, NOT PER FRAME. Blink keeps the bits per frame and propagates them up the
//! ancestor chain (and, for same-origin frames, across); this engine dispatches all trusted input into
//! the page's runtime and keeps one record for it. A frame's own `navigator.userActivation` therefore
//! reads the page's, which is a superset of Chrome's answer for a cross-origin frame the user never
//! touched. Named here rather than discovered.

use std::time::{Duration, Instant};

use deno_core::v8;

/// Blink's `kActivationLifespan` for every platform but Android (`user_activation_state.h:23`).
pub const ACTIVATION_LIFESPAN: Duration = Duration::from_secs(5);

/// The page's activation record. `last` is when the transient activation was last set; `None` once it
/// has been consumed (or never was).
#[derive(Default)]
pub struct UserActivationState {
    sticky: std::cell::Cell<bool>,
    last: std::cell::Cell<Option<Instant>>,
}

fn with_state<R>(scope: &mut v8::PinScope, f: impl FnOnce(&UserActivationState) -> R) -> R {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if op_state.borrow().try_borrow::<UserActivationState>().is_none() {
        op_state.borrow_mut().put(UserActivationState::default());
    }
    let st = op_state.borrow();
    f(st.borrow::<UserActivationState>())
}

/// `NotifyUserActivation`: an activation-triggering input event happened (a non-modifier `keydown`,
/// a pointer press). Sets both bits and restarts the transient window. THE SHARED HOOK: the keyboard
/// path (`typing.rs`) and the mouse/pointer path call this, and nothing else sets activation.
pub fn notify_user_activation(scope: &mut v8::PinScope) {
    with_state(scope, |s| {
        s.sticky.set(true);
        s.last.set(Some(Instant::now()));
    });
}

/// `HasTransientUserActivation`: an activation happened less than [`ACTIVATION_LIFESPAN`] ago and has
/// not been consumed since.
pub fn has_transient_user_activation(scope: &mut v8::PinScope) -> bool {
    with_state(scope, |s| s.last.get().is_some_and(|t| t.elapsed() < ACTIVATION_LIFESPAN))
}

/// `HasStickyUserActivation`: an activation has EVER happened in this document.
pub fn has_sticky_user_activation(scope: &mut v8::PinScope) -> bool {
    with_state(scope, |s| s.sticky.get())
}

/// `ConsumeTransientUserActivation`: spend the transient activation, answering whether there was one
/// to spend. An activation-consuming API (a popup `window.open`, `requestFullscreen`, ...) calls this;
/// the sticky bit is never consumed.
pub fn consume_transient_user_activation(scope: &mut v8::PinScope) -> bool {
    let had = has_transient_user_activation(scope);
    with_state(scope, |s| s.last.set(None));
    had
}

/// `navigator.userActivation.hasBeenActive`.
pub(crate) fn cb_has_been_active(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let b = has_sticky_user_activation(scope);
    rv.set_bool(b);
}

/// `navigator.userActivation.isActive`.
pub(crate) fn cb_is_active(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let b = has_transient_user_activation(scope);
    rv.set_bool(b);
}
