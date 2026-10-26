//! `core/animation/animation.rs` — the `Animation` interface: the 25 members Brave 151 records on
//! `Animation.prototype`, over the pure playback model in `page::animation`.
//!
//! The 25 rows of the [`ANIMATION`] table at the bottom of this file are the interface's ONE
//! definition site: names, order, kinds, lengths, `required` counts, nullability
//! and descriptor flags VERBATIM from the `install/generated_registry.rs` block they replace, and a
//! Rust body per row. THE installer (`install::live::install_window_animations`) mounts them, so the
//! descriptor (name, length, enumerable, configurable, `[native code]`) is the capture's by
//! construction and no `Object.defineProperty` of a Web API member survives anywhere.
//!
//! `onfinish` / `oncancel` / `onremove` are the `event_handler` kit -- the real `op_et_on_getter` /
//! `op_et_on_setter` pair over the native `EventTarget` handler store, which is what the registry row
//! already named and what the dispatch path below finds the handlers through.

use deno_core::v8;

use crate::binding::protos::{call_method, get_prop, require_args, str_val, throw_dom, throw_type_error};
use crate::install::MemberDef;

use super::{
    anim_handle, document_timeline, effect_end, effect_handle, row_get, row_get_set, row_method, set_anim_handle,
    timeline_now, with_store, AnimRecord, PROTO_ANIMATION,
};
use page::animation::{compute_timing, PlayState, PlaybackError, ReplaceState};

/// The animation's current time, for the effect's `getComputedTiming()` (the effect's local time IS
/// its animation's current time — there is no per-effect time offset in this model).
pub(crate) fn current_time_of(scope: &mut v8::PinScope, handle: u32) -> Option<f64> {
    let tt = timeline_now(scope);
    with_store(scope, |s| {
        let anims = s.anims.borrow();
        let a = anims.get(&handle)?;
        a.playback.current_time(a.has_timeline.then_some(tt).flatten())
    })
}

/// The timeline time this animation is measured against: the document timeline's current time, or
/// `None` when the page detached the animation from its timeline.
fn timeline_time(scope: &mut v8::PinScope, handle: u32) -> Option<f64> {
    let has = with_store(scope, |s| s.anims.borrow().get(&handle).map(|a| a.has_timeline)).unwrap_or(false);
    if !has {
        return None;
    }
    timeline_now(scope)
}

pub(crate) fn play_state_of(scope: &mut v8::PinScope, handle: u32) -> PlayState {
    let tt = timeline_time(scope, handle);
    with_store(scope, |s| {
        let end = effect_end(s, handle);
        s.anims
            .borrow()
            .get(&handle)
            .map(|a| if a.cancelled { PlayState::Idle } else { a.playback.play_state(tt, end) })
            .unwrap_or(PlayState::Idle)
    })
}

/// Create the record + wire the JS object to it. Shared by `new Animation(...)` and `animate()`.
pub(crate) fn make_animation<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    this: Option<v8::Local<'s, v8::Object>>,
    effect: Option<u32>,
    has_timeline: bool,
) -> Option<(v8::Local<'s, v8::Object>, u32)> {
    let obj = match this {
        Some(o) => o,
        None => super::new_on_proto(scope, PROTO_ANIMATION)?,
    };
    let g = v8::Global::new(scope, obj);
    let handle = with_store(scope, |s| {
        let h = s.alloc();
        s.anims.borrow_mut().insert(
            h,
            AnimRecord {
                playback: Default::default(),
                id: String::new(),
                effect,
                has_timeline,
                obj: Some(g),
                finished: None,
                finished_settled: false,
                ready: None,
                cancelled: false,
                origin: super::css::Origin::Script,
                css_events: Default::default(),
                css_paused: false,
            },
        );
        s.order.borrow_mut().push(h);
        if let Some(e) = effect {
            if let Some(rec) = s.effects.borrow_mut().get_mut(&e) {
                rec.animation = Some(h);
            }
        }
        h
    });
    set_anim_handle(scope, obj, handle);
    Some((obj, handle))
}

// ── finishing: the promise and the event ────────────────────────────────────────────────────────

/// Resolve the `finished` promise and fire `finish` the first time an animation is observed in its
/// finished state.
///
/// There is no per-frame "update animations and send events" step in this engine (no vsync), so the
/// notification happens on OBSERVATION: every member access runs this first. `finish()` therefore
/// resolves the promise synchronously, and an animation that runs out naturally resolves it the next
/// time anything looks at it — which is the first moment a page could tell either way.
pub(crate) fn update_finished(scope: &mut v8::PinScope, handle: u32) {
    let tt = timeline_time(scope, handle);
    let notify = with_store(scope, |s| {
        let end = effect_end(s, handle);
        let mut anims = s.anims.borrow_mut();
        match anims.get_mut(&handle) {
            Some(a) if !a.cancelled => a.playback.take_finish_notification(tt, end),
            _ => false,
        }
    });
    if !notify {
        return;
    }
    let (resolver, obj) = with_store(scope, |s| {
        let mut anims = s.anims.borrow_mut();
        match anims.get_mut(&handle) {
            Some(a) => {
                let r = if a.finished_settled { None } else { a.finished.clone() };
                a.finished_settled = true;
                (r, a.obj.clone())
            }
            None => (None, None),
        }
    });
    let Some(obj) = obj else { return };
    let this = v8::Local::new(scope, &obj);
    if let Some(rg) = resolver {
        let r = v8::Local::new(scope, &rg);
        let v: v8::Local<v8::Value> = this.into();
        r.resolve(scope, v);
    }
    fire(scope, this, "finish", handle);
}

/// Dispatch an `AnimationPlaybackEvent` of `kind` at the animation. The event carries the current
/// and timeline times the spec gives it, so `e.currentTime` in an `onfinish` handler is real.
fn fire(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, kind: &str, handle: u32) {
    let current = current_time_of(scope, handle);
    let tl = timeline_time(scope, handle);
    let Some(ev) = super::animation_event::new_playback_event(scope, kind, current, tl) else { return };
    let recv: v8::Local<v8::Value> = this.into();
    let arg: v8::Local<v8::Value> = ev.into();
    crate::binding::protos::engine_call(scope, recv, "dispatchEvent", &[arg]);
}

// ── the constructor ─────────────────────────────────────────────────────────────────────────────

pub(crate) fn cb_anim_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("Animation"));
        return;
    }
    // `constructor(optional AnimationEffect? effect = null, optional AnimationTimeline? timeline)` —
    // both optional, so `new Animation()` is legal and is what Brave 151's recorded answers describe.
    let effect_v = args.get(0);
    let effect = if effect_v.is_null_or_undefined() {
        None
    } else {
        match effect_handle(scope, effect_v) {
            Some(h) => Some(h),
            None => {
                throw_type_error(scope, "Failed to construct 'Animation': parameter 1 is not of type 'AnimationEffect'.");
                return;
            }
        }
    };
    // An OMITTED timeline is the document timeline; an explicit `null` is no timeline at all. WebIDL
    // cannot express that difference in the default, so the argument count is what distinguishes them
    // — and `new Animation().timeline` is recorded as a DocumentTimeline, not null.
    let has_timeline = if args.length() < 2 { true } else { !args.get(1).is_null() };
    if has_timeline {
        document_timeline(scope);
    }
    let this = args.this();
    if make_animation(scope, Some(this), effect, has_timeline).is_some() {
        rv.set(this.into());
    }
}

// ── members ─────────────────────────────────────────────────────────────────────────────────────

fn this_anim(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, member: &str) -> Option<u32> {
    let this: v8::Local<v8::Value> = args.this().into();
    match anim_handle(scope, this) {
        Some(h) => Some(h),
        None => {
            throw_type_error(scope, &format!("Failed to read the '{member}' property: Illegal invocation"));
            None
        }
    }
}

fn set_num_rv(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, v: Option<f64>) {
    match v {
        Some(n) => rv.set(v8::Number::new(scope, n).into()),
        None => rv.set_null(),
    }
}

/// Create-or-return one of the two promises. `settled_now` resolves it immediately (the `ready`
/// promise, and a `finished` promise asked for after the animation already finished).
fn promise<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    handle: u32,
    which: Which,
    settle_now: bool,
) -> Option<v8::Local<'s, v8::Value>> {
    let existing = with_store(scope, |s| {
        let anims = s.anims.borrow();
        let a = anims.get(&handle)?;
        match which {
            Which::Ready => a.ready.clone(),
            Which::Finished => a.finished.clone(),
        }
    });
    if let Some(g) = existing {
        let r = v8::Local::new(scope, &g);
        return Some(r.get_promise(scope).into());
    }
    let resolver = v8::PromiseResolver::new(scope)?;
    let g = v8::Global::new(scope, resolver);
    let obj = with_store(scope, |s| {
        let mut anims = s.anims.borrow_mut();
        let a = anims.get_mut(&handle)?;
        match which {
            Which::Ready => a.ready = Some(g),
            Which::Finished => {
                a.finished = Some(g);
                if settle_now {
                    a.finished_settled = true;
                }
            }
        }
        a.obj.clone()
    })?;
    if settle_now {
        let this = v8::Local::new(scope, &obj);
        let v: v8::Local<v8::Value> = this.into();
        resolver.resolve(scope, v);
    }
    Some(resolver.get_promise(scope).into())
}

#[derive(Clone, Copy)]
enum Which {
    Ready,
    Finished,
}

fn cb_anim_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    let member = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(h) = this_anim(scope, &args, &member) else { return };
    update_finished(scope, h);
    match member.as_str() {
        "effect" => {
            // The effect's JS object is not stored: an effect is reached through the animation, and
            // `KeyframeEffect` objects are created by `animate()`/`new KeyframeEffect()` only. Look
            // up the object we minted for the handle.
            let g = with_store(scope, |s| {
                let anims = s.anims.borrow();
                let e = anims.get(&h)?.effect?;
                s.effect_objs.borrow().get(&e).cloned()
            });
            match g {
                Some(g) => {
                    let o = v8::Local::new(scope, &g);
                    rv.set(o.into());
                }
                None => rv.set_null(),
            }
        }
        "timeline" => {
            let has = with_store(scope, |s| s.anims.borrow().get(&h).map(|a| a.has_timeline)).unwrap_or(false);
            match has.then(|| document_timeline(scope)).flatten() {
                Some(t) => rv.set(t.into()),
                None => rv.set_null(),
            }
        }
        "startTime" => {
            let v = with_store(scope, |s| s.anims.borrow().get(&h).and_then(|a| a.playback.start_time));
            set_num_rv(scope, &mut rv, v);
        }
        "currentTime" => {
            let v = current_time_of(scope, h);
            set_num_rv(scope, &mut rv, v);
        }
        "playbackRate" => {
            let v = with_store(scope, |s| s.anims.borrow().get(&h).map(|a| a.playback.playback_rate)).unwrap_or(1.0);
            rv.set(v8::Number::new(scope, v).into());
        }
        // `rangeStart` / `rangeEnd` are the scroll-timeline attachment range. A document timeline is
        // not progress-based, so the range is the WebIDL default `TimelineRangeOffset` — the string
        // "normal" — and nothing else can be observed from one.
        "rangeStart" | "rangeEnd" => {
            let v = str_val(scope, "normal");
            rv.set(v);
        }
        "playState" => {
            let s = play_state_of(scope, h).as_str();
            let v = str_val(scope, s);
            rv.set(v);
        }
        "replaceState" => {
            let s = with_store(scope, |s| s.anims.borrow().get(&h).map(|a| a.playback.replace_state))
                .unwrap_or(ReplaceState::Active);
            let v = str_val(scope, s.as_str());
            rv.set(v);
        }
        // No pending-task queue exists in this engine (see page::animation's header): every playback
        // change applies synchronously, so there is never a moment at which an animation is pending.
        "pending" => rv.set(v8::Boolean::new(scope, false).into()),
        "id" => {
            let s = with_store(scope, |s| s.anims.borrow().get(&h).map(|a| a.id.clone())).unwrap_or_default();
            let v = str_val(scope, &s);
            rv.set(v);
        }
        "ready" => {
            if let Some(p) = promise(scope, h, Which::Ready, true) {
                rv.set(p);
            }
        }
        "finished" => {
            let done = play_state_of(scope, h) == PlayState::Finished;
            if let Some(p) = promise(scope, h, Which::Finished, done) {
                rv.set(p);
            }
        }
        "overallProgress" => {
            let tt = timeline_time(scope, h);
            let v = with_store(scope, |s| {
                let anims = s.anims.borrow();
                let a = anims.get(&h)?;
                if a.cancelled {
                    return None;
                }
                let effects = s.effects.borrow();
                let timing = a.effect.and_then(|e| effects.get(&e)).map(|e| e.timing.clone())?;
                a.playback.overall_progress(tt, &timing)
            });
            set_num_rv(scope, &mut rv, v);
        }
        _ => {}
    }
}

fn cb_anim_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let member = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(h) = this_anim(scope, &args, &member) else { return };
    let value = args.get(0);
    let tt = timeline_time(scope, h);
    match member.as_str() {
        "effect" => {
            let e = if value.is_null_or_undefined() {
                None
            } else {
                match effect_handle(scope, value) {
                    Some(e) => Some(e),
                    None => {
                        throw_type_error(scope, "Failed to set the 'effect' property on 'Animation': The provided value is not of type 'AnimationEffect'.");
                        return;
                    }
                }
            };
            with_store(scope, |s| {
                let previous = s.anims.borrow().get(&h).and_then(|a| a.effect);
                let mut effects = s.effects.borrow_mut();
                if let Some(p) = previous {
                    if let Some(rec) = effects.get_mut(&p) {
                        rec.animation = None;
                    }
                }
                if let Some(e) = e {
                    if let Some(rec) = effects.get_mut(&e) {
                        rec.animation = Some(h);
                    }
                }
                if let Some(a) = s.anims.borrow_mut().get_mut(&h) {
                    a.effect = e;
                }
            });
        }
        "timeline" => {
            let has = !value.is_null_or_undefined();
            if has {
                document_timeline(scope);
            }
            with_store(scope, |s| {
                if let Some(a) = s.anims.borrow_mut().get_mut(&h) {
                    a.has_timeline = has;
                }
            });
        }
        "startTime" => {
            let v = if value.is_null_or_undefined() { None } else { value.number_value(scope).filter(|n| n.is_finite()) };
            with_store(scope, |s| {
                if let Some(a) = s.anims.borrow_mut().get_mut(&h) {
                    a.playback.set_start_time(tt, v);
                    a.cancelled = false;
                }
            });
        }
        "currentTime" => {
            if value.is_null_or_undefined() {
                // Setting currentTime to null on a running animation is an InvalidStateError; on a
                // paused/idle one it just clears the hold time.
                let running = play_state_of(scope, h) == PlayState::Running;
                if running {
                    throw_dom(scope, "Failed to set the 'currentTime' property on 'Animation': currentTime may not be changed from resolved to unresolved", "TypeError");
                    return;
                }
                with_store(scope, |s| {
                    if let Some(a) = s.anims.borrow_mut().get_mut(&h) {
                        a.playback.hold_time = None;
                        a.playback.start_time = None;
                    }
                });
                return;
            }
            let Some(n) = value.number_value(scope).filter(|n| n.is_finite()) else { return };
            with_store(scope, |s| {
                if let Some(a) = s.anims.borrow_mut().get_mut(&h) {
                    a.playback.set_current_time(tt, n);
                    a.cancelled = false;
                }
            });
        }
        "playbackRate" => {
            // WebIDL `double`: `a.playbackRate = "1"` is 1, which Brave 151 records.
            let Some(n) = value.number_value(scope).filter(|n| n.is_finite()) else { return };
            with_store(scope, |s| {
                if let Some(a) = s.anims.borrow_mut().get_mut(&h) {
                    a.playback.set_playback_rate(tt, n);
                }
            });
        }
        // Assigning a range to an animation on a document timeline has nothing to attach to; the
        // value is accepted and the getter keeps reporting the default, exactly as a monotonic
        // timeline leaves it.
        "rangeStart" | "rangeEnd" => {}
        "id" => {
            let s = value.to_rust_string_lossy(scope);
            with_store(scope, |st| {
                if let Some(a) = st.anims.borrow_mut().get_mut(&h) {
                    a.id = s;
                }
            });
        }
        _ => {}
    }
}

/// `InvalidStateError` with the message Blink uses, so `e.message` matches too.
fn invalid_state(scope: &mut v8::PinScope, what: &str) {
    throw_dom(scope, what, "InvalidStateError");
}

fn cb_anim_method<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let member = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(h) = this_anim(scope, &args, &member) else { return };
    let tt = timeline_time(scope, h);
    match member.as_str() {
        "play" => {
            let r = with_store(scope, |s| {
                let end = effect_end(s, h);
                let mut anims = s.anims.borrow_mut();
                match anims.get_mut(&h) {
                    Some(a) => {
                        a.cancelled = false;
                        Some(a.playback.play(tt, end))
                    }
                    None => None,
                }
            });
            if let Some(Err(PlaybackError::InvalidState)) = r {
                invalid_state(scope, "Cannot play reversed Animation with infinite target effect end.");
                return;
            }
            reset_finished(scope, h);
        }
        "pause" => {
            let r = with_store(scope, |s| {
                let end = effect_end(s, h);
                let mut anims = s.anims.borrow_mut();
                anims.get_mut(&h).map(|a| a.playback.pause(tt, end))
            });
            if let Some(Err(PlaybackError::InvalidState)) = r {
                invalid_state(scope, "Cannot pause reversed Animation with infinite target effect end.");
            }
        }
        "finish" => {
            let r = with_store(scope, |s| {
                let end = effect_end(s, h);
                let mut anims = s.anims.borrow_mut();
                anims.get_mut(&h).map(|a| {
                    a.cancelled = false;
                    a.playback.finish(tt, end)
                })
            });
            match r {
                Some(Err(PlaybackError::InvalidState)) => {
                    invalid_state(scope, "Cannot finish Animation with an infinite target effect end.");
                    return;
                }
                _ => {}
            }
            update_finished(scope, h);
        }
        "cancel" => {
            let was_active = play_state_of(scope, h) != PlayState::Idle;
            with_store(scope, |s| {
                if let Some(a) = s.anims.borrow_mut().get_mut(&h) {
                    a.playback.cancel();
                    a.cancelled = true;
                }
            });
            reject_finished(scope, h);
            if was_active {
                if let Some(g) = with_store(scope, |s| s.anims.borrow().get(&h).and_then(|a| a.obj.clone())) {
                    let this = v8::Local::new(scope, &g);
                    fire(scope, this, "cancel", h);
                }
            }
        }
        "reverse" => {
            let r = with_store(scope, |s| {
                let end = effect_end(s, h);
                let mut anims = s.anims.borrow_mut();
                anims.get_mut(&h).map(|a| {
                    a.cancelled = false;
                    a.playback.reverse(tt, end)
                })
            });
            if let Some(Err(PlaybackError::InvalidState)) = r {
                invalid_state(scope, "Cannot play reversed Animation with infinite target effect end.");
                return;
            }
            reset_finished(scope, h);
        }
        "updatePlaybackRate" => {
            // `updatePlaybackRate(double playbackRate)` — one REQUIRED argument. Brave 151 records
            // `updatePlaybackRate()` as a TypeError and `updatePlaybackRate(1)` as undefined.
            if !require_args(scope, &args, 1, "Animation", "updatePlaybackRate") {
                return;
            }
            let Some(n) = args.get(0).number_value(scope).filter(|n| n.is_finite()) else {
                throw_type_error(scope, "Failed to execute 'updatePlaybackRate' on 'Animation': The provided double value is non-finite.");
                return;
            };
            with_store(scope, |s| {
                if let Some(a) = s.anims.borrow_mut().get_mut(&h) {
                    a.playback.update_playback_rate(tt, n);
                }
            });
        }
        "persist" => {
            with_store(scope, |s| {
                if let Some(a) = s.anims.borrow_mut().get_mut(&h) {
                    a.playback.replace_state = ReplaceState::Persisted;
                }
            });
        }
        "commitStyles" => commit_styles(scope, h),
        _ => {}
    }
}

/// Playing/reversing an animation that had already finished starts a NEW finished run: the old
/// promise is left resolved and the next finish gets a fresh one, which is what the spec's
/// "current finished promise" reset does.
fn reset_finished(scope: &mut v8::PinScope, h: u32) {
    with_store(scope, |s| {
        if let Some(a) = s.anims.borrow_mut().get_mut(&h) {
            if a.finished_settled {
                a.finished = None;
                a.finished_settled = false;
            }
        }
    });
}

/// `cancel()` rejects a pending `finished` promise with an `AbortError`.
///
/// The rejection is marked HANDLED before it is delivered. That is not a way to hide it: a page's
/// own `.catch` still runs, and `finished` still rejects. It stops deno_core's unhandled-rejection
/// path from treating a cancelled animation nobody awaited as a fatal isolate event — which it would
/// be here, where an unhandled rejection aborts the run rather than printing a console warning.
fn reject_finished(scope: &mut v8::PinScope, h: u32) {
    let resolver = with_store(scope, |s| {
        let mut anims = s.anims.borrow_mut();
        let a = anims.get_mut(&h)?;
        if a.finished_settled {
            return None;
        }
        a.finished_settled = true;
        a.finished.take()
    });
    let Some(g) = resolver else { return };
    let r = v8::Local::new(scope, &g);
    let promise = r.get_promise(scope);
    let Some(err) = crate::binding::protos::make_dom_exc(scope, "The user aborted a request.", "AbortError") else {
        return;
    };
    r.reject(scope, err);
    promise.mark_as_handled();
}

/// `commitStyles()`: write the effect's value at the current time into the target's inline style.
///
/// The error is Blink's own, recorded: with no styleable target it is a `NoModificationAllowedError`,
/// not the `InvalidStateError` an early draft of the spec named.
fn commit_styles(scope: &mut v8::PinScope, h: u32) {
    let target = with_store(scope, |s| {
        let anims = s.anims.borrow();
        let e = anims.get(&h)?.effect?;
        s.effects.borrow().get(&e).and_then(|e| e.target_obj.clone())
    });
    let Some(target) = target else {
        throw_dom(
            scope,
            "Failed to execute 'commitStyles' on 'Animation': Animation is not associated with a styleable target.",
            "NoModificationAllowedError",
        );
        return;
    };
    let local = current_time_of(scope, h);
    let (timing, frames) = match with_store(scope, |s| {
        let anims = s.anims.borrow();
        let e = anims.get(&h)?.effect?;
        s.effects.borrow().get(&e).map(|e| (e.timing.clone(), e.keyframes.clone()))
    }) {
        Some(v) => v,
        None => return,
    };
    let progress = compute_timing(&timing, local).progress.unwrap_or(1.0);
    let el = v8::Local::new(scope, &target);
    let Some(style) = get_prop(scope, el, "style").filter(|v| v.is_object()) else { return };
    // Collect every property the keyframes touch, then write the value that property has at the
    // effect's current progress.
    let mut names: Vec<String> = Vec::new();
    for f in &frames {
        for (_idl, css, _v) in &f.props {
            if !names.contains(css) {
                names.push(css.clone());
            }
        }
    }
    for css in names {
        let Some(value) = value_at(&frames, progress, &css) else { continue };
        let k = str_val(scope, &css);
        let v = str_val(scope, &value);
        call_method(scope, style, "setProperty", &[k, v]);
    }
}

/// The value a property has at `progress`: linearly interpolated when both bracketing keyframes are
/// a plain number with the same (possibly empty) unit, and the bracketing keyframe's own value
/// otherwise. This is deliberately not a full CSS value interpolator — `commitStyles` is the only
/// member of this unit that writes style at all, and a wrong colour mix would be a worse answer than
/// the endpoint value it is between.
fn value_at(frames: &[super::Keyframe], progress: f64, css: &str) -> Option<String> {
    let with_prop: Vec<(f64, &str)> = frames
        .iter()
        .filter_map(|f| f.props.iter().find(|(_, c, _)| c == css).map(|(_, _, v)| (f.computed_offset, v.as_str())))
        .collect();
    if with_prop.is_empty() {
        return None;
    }
    let last = with_prop.last().unwrap();
    if progress >= last.0 {
        return Some(last.1.to_string());
    }
    if progress <= with_prop[0].0 {
        return Some(with_prop[0].1.to_string());
    }
    let i = with_prop.iter().position(|(o, _)| *o > progress)?;
    let (o0, v0) = with_prop[i - 1];
    let (o1, v1) = with_prop[i];
    let span = o1 - o0;
    let t = if span > 0.0 { (progress - o0) / span } else { 0.0 };
    match (split_number(v0), split_number(v1)) {
        (Some((a, ua)), Some((b, ub))) if ua == ub => Some(format!("{}{}", a + (b - a) * t, ua)),
        _ => Some(v0.to_string()),
    }
}

/// `"12.5px"` -> `(12.5, "px")`; `None` when the value does not start with a number.
fn split_number(s: &str) -> Option<(f64, &str)> {
    let end = s
        .char_indices()
        .find(|(i, c)| !(c.is_ascii_digit() || *c == '.' || ((*c == '-' || *c == '+') && *i == 0)))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(end);
    num.parse::<f64>().ok().map(|n| (n, unit))
}

// ── the table ───────────────────────────────────────────────────────────────────────────────────

/// Mount one `Animation` row. Fourteen getters, eight of them paired with a setter, and eight
/// methods -- all three callbacks read the member name out of the function's `data`, which is what
/// lets 22 rows name three bodies.
pub fn install_animation(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        // The writable attributes: a real getter/setter pair.
        "effect" | "timeline" | "startTime" | "currentTime" | "playbackRate" | "rangeStart" | "rangeEnd" | "id" => {
            row_get_set(scope, proto, member, cb_anim_get, cb_anim_set)
        }
        // The read-only ones: a getter and nothing else, so assigning throws in strict mode exactly
        // as it does in Chrome.
        "playState" | "replaceState" | "pending" | "finished" | "ready" | "overallProgress" => {
            row_get(scope, proto, member, cb_anim_get)
        }
        "cancel" | "commitStyles" | "finish" | "pause" | "persist" | "play" | "reverse" | "updatePlaybackRate" => {
            row_method(scope, proto, member, cb_anim_method)
        }
        other => debug_assert!(false, "install_animation: unexpected member {other}"),
    }
}

webapi::interface! {
    "Animation" : "EventTarget" [value, window] {
    ctor            "Animation" len 0                        = stub,
    attr            "effect" : Object nullable               = install_animation,
    attr            "timeline" : Object nullable             = install_animation,
    attr            "startTime" : Object nullable            = install_animation,
    attr            "currentTime" : Object nullable          = install_animation,
    attr            "playbackRate" : Object                  = install_animation,
    attr            "rangeStart" : Object                    = install_animation,
    attr            "rangeEnd" : Object                      = install_animation,
    attr            "playState" : Object readonly            = install_animation,
    attr            "replaceState" : Object readonly         = install_animation,
    attr            "pending" : Object readonly              = install_animation,
    attr            "id" : Object                            = install_animation,
    handler         "onfinish" nullable                      = event_handler,
    handler         "oncancel" nullable                      = event_handler,
    handler         "onremove" nullable                      = event_handler,
    attr            "finished" : Promise readonly            = install_animation,
    attr            "ready" : Promise readonly               = install_animation,
    method          "cancel" : Object                        = install_animation,
    method          "commitStyles" : Object                  = install_animation,
    method          "finish" : Object                        = install_animation,
    method          "pause" : Object                         = install_animation,
    method          "persist" : Object                       = install_animation,
    method          "play" : Object                          = install_animation,
    method          "reverse" : Object                       = install_animation,
    method          "updatePlaybackRate" : Object len 1 required 1 = install_animation,
    attr            "overallProgress" : Object readonly nullable  = install_animation
    }
}
