//! The Web Animations family.

use deno_core::v8;

use super::rows::build_newable;
use crate::install::installer::{self, Flavor};

/// The nine Web Animations interfaces from their `interface!` tables, in the WINDOW.
///
/// `Animation` (25 rows), `AnimationEffect` (3) + `KeyframeEffect` (5), `AnimationTimeline` (2) +
/// `DocumentTimeline` (0) + `ScrollTimeline` (2), and `AnimationEvent` (5) + `TransitionEvent` (5) +
/// `AnimationPlaybackEvent` (2) -- 49 rows that were `dom/09j-animation.js` (143 lines, five
/// `Object.defineProperty` sites and eighteen op-builder round-trips). The bodies were ALREADY Rust
/// (`core/animation/`), so this moves their DEFINITION SITE: every row names the callback the JS was
/// installing and no answer changes.
///
/// WHY THIS RUNS BEFORE [`super::install_window_abort_style`], and not beside the lanes above it.
/// Three interfaces the registry still owns inherit from two of these -- `ViewTimeline` from
/// `ScrollTimeline`, `CSSAnimation` and `CSSTransition` from `Animation`. The registry's chain pass
/// runs during the ESM layer, long before this, and cannot resolve a parent the global does not name
/// yet, so it leaves all three on the bare `Object.prototype`. [`super::rows::rechain_orphans`], which
/// `install_window_abort_style` calls at its end, is the repair -- and it can only repair a parent that
/// exists by the time it runs. Installed after L12 the three would keep a one-deep chain where the
/// capture records two and three (`ViewTimeline.proto` is `["ScrollTimeline","AnimationTimeline"]`),
/// and `new ViewTimeline().currentTime` would be `undefined` instead of null.
///
/// NOTHING ELSE WRITES THESE NINE PROTOTYPES. Every remaining `.js` file in both crates, both Rust
/// bootstrap strings and `host.js` were grepped for all nine interface names: the only hits were in
/// `dom/09j-animation.js`, which this lane deletes. The interfaces also leave `generated_registry.rs`
/// with the tables, so `finalize_surface`'s arity pass does not reach them -- the row's own guard
/// (`Animation.updatePlaybackRate`, `KeyframeEffect.setKeyframes`) is what enforces the WebIDL
/// required-argument count, applied at install time by `core::animation::row_method`. (They left
/// `generated_order.rs` too; that second table and its enumeration-order pass are deleted.)
///
/// Window-only: the bundle's `worker.surface.json` lists none of the nine.
pub fn install_window_animations<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::animation::animation::{cb_anim_ctor, ANIMATION};
    use crate::core::animation::animation_event::{
        cb_animation_event_ctor, cb_playback_event_ctor, cb_transition_event_ctor, ANIMATIONEVENT,
        ANIMATIONPLAYBACKEVENT, TRANSITIONEVENT,
    };
    use crate::core::animation::document_timeline::{
        cb_document_timeline_ctor, cb_scroll_timeline_ctor, ANIMATIONTIMELINE, DOCUMENTTIMELINE, SCROLLTIMELINE,
    };
    use crate::core::animation::keyframe_effect::{cb_kfe_ctor, ANIMATIONEFFECT, KEYFRAMEEFFECT};

    let mut ledger = installer::take_ledger(scope);
    // WebIDL inheritance order: each abstract base before the concrete subclasses spliced onto it.
    // `AnimationEffect` and `AnimationTimeline` are the two abstract ones -- a real constructor that
    // throws "Illegal constructor", which is the TypeError the bundle's oracle records for both.
    for def in [&ANIMATIONEFFECT, &ANIMATIONTIMELINE] {
        // On the page, on a blueprint hit, put lazily instead (`installer::put_lazy_table`)
        if installer::put_lazy_table(scope, global, def, &mut ledger) {
            continue;
        }
        let built = installer::create_interface(scope, global, def, def.name, None, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] animations install gap: {}.{}", g.interface, g.member);
        }
    }
    build_newable(scope, global, &KEYFRAMEEFFECT, cb_kfe_ctor, Some("AnimationEffect"), None, &mut ledger);
    build_newable(scope, global, &DOCUMENTTIMELINE, cb_document_timeline_ctor, Some("AnimationTimeline"), None, &mut ledger);
    build_newable(scope, global, &SCROLLTIMELINE, cb_scroll_timeline_ctor, Some("AnimationTimeline"), None, &mut ledger);
    build_newable(scope, global, &ANIMATION, cb_anim_ctor, Some("EventTarget"), None, &mut ledger);
    build_newable(scope, global, &ANIMATIONEVENT, cb_animation_event_ctor, Some("Event"), None, &mut ledger);
    build_newable(scope, global, &TRANSITIONEVENT, cb_transition_event_ctor, Some("Event"), None, &mut ledger);
    build_newable(scope, global, &ANIMATIONPLAYBACKEVENT, cb_playback_event_ctor, Some("Event"), None, &mut ledger);
    installer::put_ledger(scope, ledger);

    // The four prototypes `core/animation/` mints instances on. They are registered from HERE rather
    // than looked up through `globalThis.Animation` at call time, so a page that overwrites the
    // global cannot change what `element.animate()` returns -- the property
    // `op_anim_register_proto(P_X, X)` gave from `dom/09j-animation.js`, now a plain Rust call from
    // the table's own install.
}
