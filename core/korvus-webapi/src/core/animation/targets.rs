//! `core/animation/targets.rs` — the entry points a page actually calls: `Element.animate()`,
//! `Element.getAnimations()` and `Document.getAnimations()`.
//!
//! `getAnimations()` is the member this whole unit exists to stop lying about. It used to be a JS
//! stub returning `[]` on `Document` and a no-op shell on `Element`, which was honest only while
//! nothing could animate. Now it returns the animations that are genuinely in effect — and it is the
//! same commit that made `animate()` return an `Animation`, because either one alone is a lie.

use deno_core::v8;

use crate::binding::protos::{arg_node_id, get_prop, require_args, throw_type_error};
use crate::binding::HostDom;

use super::animation::{make_animation, play_state_of};
use super::keyframe_effect::{apply_timing, make_effect, process_keyframes};
use super::{document_timeline, with_store};
use page::animation::{PlayState, Timing};

/// `Element.animate(keyframes, options)`.
pub(crate) fn cb_element_animate<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    let Some(node_id) = arg_node_id(scope, this) else {
        throw_type_error(scope, "Failed to execute 'animate' on 'Element': Illegal invocation");
        return;
    };
    // `object? keyframes` is REQUIRED: `el.animate()` is a TypeError. Brave 151 records it.
    if !require_args(scope, &args, 1, "Element", "animate") {
        return;
    }
    // `(unrestricted double or KeyframeAnimationOptions) options` is converted with the other arguments -- before
    // the keyframes are processed (Chrome 150): the `EffectTiming` members, `composite`,
    // `pseudoElement`, then `id`, `rangeEnd`, `rangeStart`, `timeline`, each read once
    let Ok(options) = super::keyframe_effect::timing_options(scope, args.get(1), "KeyframeAnimationOptions", "Failed to execute 'animate' on 'Element': ") else { return };
    let frames = match process_keyframes(scope, args.get(0), "Element") {
        Ok(f) => f,
        Err(msg) => {
            throw_type_error(scope, &msg);
            return;
        }
    };
    let mut timing = Timing::default();
    if let Err(msg) = apply_timing(scope, options, &mut timing) {
        throw_type_error(scope, &msg);
        return;
    }
    // `KeyframeAnimationOptions` adds `id` on top of `KeyframeEffectOptions`.
    let id = v8::Local::<v8::Object>::try_from(options)
        .ok()
        .and_then(|o| get_prop(scope, o, "id"))
        .filter(|v| !v.is_undefined())
        .map(|v| v.to_rust_string_lossy(scope));

    let target = args.this();
    let Some(effect) = make_effect(scope, None, Some(target), Some(node_id), frames, timing) else { return };
    let Some(handle) = super::effect_handle(scope, effect.into()) else { return };
    document_timeline(scope);
    let Some((anim, anim_handle)) = make_animation(scope, None, Some(handle), true) else { return };
    if let Some(id) = id {
        with_store(scope, |s| {
            if let Some(a) = s.anims.borrow_mut().get_mut(&anim_handle) {
                a.id = id;
            }
        });
    }
    // `animate()` PLAYS: the returned animation's play state is "running", not "idle". That is the
    // whole point of the sugar over `new Animation(new KeyframeEffect(...))`.
    let tt = super::timeline_now(scope);
    with_store(scope, |s| {
        let end = super::effect_end(s, anim_handle);
        if let Some(a) = s.anims.borrow_mut().get_mut(&anim_handle) {
            let _ = a.playback.play(tt, end);
        }
    });
    rv.set(anim.into());
}

/// Is this animation one `getAnimations()` should report?
///
/// The spec's word is "relevant", and what it means was MEASURED rather than reasoned, because the
/// comment that used to stand here had it backwards: it said "a `finished` one with the default
/// `fill: none` stays in until it is cancelled (Chrome keeps a finished, non-filling animation
/// listed until it is replaced)". Chrome does not. Driven over CDP against stock Chrome
/// 150.0.7871.46, one `element.animate(…, {duration: 100, fill})` per fill mode, paused and seeked,
/// asking `document.getAnimations().includes(a)` at each point:
///
/// ```text
///   fill        t=0    t=50   t=100  t=500   after cancel()
///   none        in     in     OUT    OUT     out
///   backwards   in     in     OUT    OUT     out
///   forwards    in     in     in     in      out
///   both        in     in     in     in      out
///
///   a CSS animation with animation-fill-mode: none behaves as `none` above
///   a DELAYED animation, fill none, seeked to 0 (its BEFORE phase): IN, with progress null
/// ```
///
/// So membership is: the effect is **in effect** (its progress is non-null — which is what
/// `forwards`/`both` keep true past the end), **or** the animation is still **current** — it has not
/// reached its active phase yet, which is the delayed row and the one case a null progress is still
/// in the list for.
///
/// The old rule reported every finished non-filling animation for ever. That is over-reporting in
/// the flattering direction on an axis scored partly by `getAnimations().length`, which is why it is
/// fixed here rather than left for the number to benefit from.
fn relevant(scope: &mut v8::PinScope, handle: u32) -> bool {
    let state = play_state_of(scope, handle);
    if state == PlayState::Idle {
        return false;
    }
    let local = super::animation::current_time_of(scope, handle);
    with_store(scope, |s| {
        let anims = s.anims.borrow();
        let Some(a) = anims.get(&handle) else { return false };
        if a.playback.replace_state == page::animation::ReplaceState::Removed {
            return false;
        }
        let Some(e) = a.effect else { return false };
        let effects = s.effects.borrow();
        let Some(effect) = effects.get(&e) else { return false };
        if effect.target.is_none() {
            return false;
        }
        let computed = page::animation::compute_timing(&effect.timing, local);
        // IN EFFECT: it is contributing a value right now.
        if computed.progress.is_some() {
            return true;
        }
        // CURRENT: it has not got there yet. Which end "not yet" is depends on the direction of
        // travel -- a reversed animation approaches its effect from the far side.
        let ahead = if a.playback.playback_rate < 0.0 { page::animation::Phase::After } else { page::animation::Phase::Before };
        computed.phase == ahead
    })
}

/// Collect the relevant animations whose effect target satisfies `keep`, in composite (creation)
/// order, as a JS Array.
fn collect<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    keep: impl Fn(&mut v8::PinScope, u32) -> bool,
) -> v8::Local<'s, v8::Array> {
    // Composite order. Creation order is the tie-break WITHIN a rung, but the rungs come first:
    // measured in Chrome 150 on a page carrying one of each, `document.getAnimations()` returned
    // `CSSTransition · CSSAnimation · CSSAnimation · Animation` -- transitions, then the animations
    // a stylesheet declared in document order, then everything script made.
    let order = with_store(scope, |s| {
        let mut order: Vec<(u8, (u32, usize), usize, u32)> = s
            .order
            .borrow()
            .iter()
            .enumerate()
            .filter_map(|(i, h)| {
                let anims = s.anims.borrow();
                let a = anims.get(h)?;
                Some((a.origin.rung(), a.origin.within(), i, *h))
            })
            .collect();
        order.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
        order.into_iter().map(|(_, _, _, h)| h).collect::<Vec<u32>>()
    });
    let mut out: Vec<v8::Global<v8::Object>> = Vec::new();
    for h in order {
        if !relevant(scope, h) {
            continue;
        }
        let target = with_store(scope, |s| {
            let anims = s.anims.borrow();
            let e = anims.get(&h)?.effect?;
            s.effects.borrow().get(&e).and_then(|e| e.target)
        });
        let Some(target) = target else { continue };
        if !keep(scope, target) {
            continue;
        }
        if let Some(g) = with_store(scope, |s| s.anims.borrow().get(&h).and_then(|a| a.obj.clone())) {
            out.push(g);
        }
    }
    let arr = v8::Array::new(scope, out.len() as i32);
    for (i, g) in out.iter().enumerate() {
        let o = v8::Local::new(scope, g);
        arr.set_index(scope, i as u32, o.into());
    }
    arr
}

/// `Element.getAnimations({ subtree })`.
pub(crate) fn cb_element_get_animations(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    let Some(node_id) = arg_node_id(scope, this) else {
        throw_type_error(scope, "Failed to execute 'getAnimations' on 'Element': Illegal invocation");
        return;
    };
    super::css::tick(scope);
    let subtree = v8::Local::<v8::Object>::try_from(args.get(0))
        .ok()
        .and_then(|o| get_prop(scope, o, "subtree"))
        .map(|v| v.boolean_value(scope))
        .unwrap_or(false);
    let arr = collect(scope, |scope, target| {
        if target == node_id {
            return true;
        }
        if !subtree {
            return false;
        }
        let rc = super::store_rc(scope);
        let st = rc.borrow();
        st.borrow::<HostDom>().0.contains(node_id, target)
    });
    rv.set(arr.into());
}

/// `Document.getAnimations()` — every relevant animation whose target is IN this document.
///
/// The connectedness test is what separates this from `Element.getAnimations()`: an animation on a
/// detached `document.createElement("div")` is the element's, not the document's. It is also why the
/// old `[]` stub could not simply stay: an element attached to the page with a running animation is
/// exactly the case it would still be answering `[]` for.
pub(crate) fn cb_document_get_animations(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    // The stylesheets' own animations exist because the style says so, and nothing has asked the
    // style yet on a page that only calls `getAnimations()`. This is where they come into being.
    super::css::tick(scope);
    let arr = collect(scope, |scope, target| {
        let rc = super::store_rc(scope);
        let st = rc.borrow();
        st.borrow::<HostDom>().0.is_connected(target)
    });
    rv.set(arr.into());
}

