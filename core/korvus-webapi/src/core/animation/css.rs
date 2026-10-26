//! `core/animation/css.rs` — the animations a STYLESHEET declares, reconciled against the one store.
//!
//! # What was missing, measured rather than assumed
//!
//! Before this file, driving a CSS-animated page through the engine over CDP gave:
//! `document.getAnimations()` → `0`; `getComputedStyle(#a).opacity` → `"1"` at t0 and again 600 ms
//! later; a `transition: opacity 3s` element that jumped from `0.25` to `0.75` in one read. And the
//! SCRIPT path was no better: an `element.animate()` with `fill: "both"` seeked to five points read
//! `opacity: "1"` at every one. The engine had the objects and none of the motion.
//!
//! # The shape
//!
//! There is ONE animation store (`super::AnimationStore`) and it holds every animation, however it
//! was created — that is what lets `getAnimations()` return script animations, CSS animations and
//! transitions in one list, and what lets `a.pause()` on a `CSSAnimation` work like any other.
//! `korvus-page` says what the stylesheets DECLARE (`Page::declarative_update`); this file turns
//! that into records, and turns the records' current values back into the cascade override the page
//! applies (`Page::set_animated`).
//!
//! # When it runs
//!
//! On OBSERVATION, not on a frame clock. This engine has no vsync, so "before anything can look" is
//! the only honest trigger — the same rule `update_finished` already follows for the `finish` event.
//! Chrome agrees that this is observable behaviour rather than a shortcut: an element appended with
//! a paused `animation` reads its first keyframe's value from `getComputedStyle` **immediately**,
//! with no frame in between, and is already in `getAnimations()` (measured).
//!
//! # Where the timing function lives, which is not where it looks like it should
//!
//! For a CSS animation Chrome puts `animation-timing-function` on **each keyframe**, and the
//! effect's own easing is **`linear`**. Measured on Chrome 150, `animation: fade 4s ease`:
//!
//!   effect.getTiming().easing        "linear"
//!   effect.getKeyframes()[i].easing  "ease"
//!   opacity at progress .25/.5/.75   0.408511 · 0.802403 · 0.960459   (one `ease`, not two)
//!
//! and a keyframe block carrying its own `animation-timing-function: steps(2)` overrides it for that
//! keyframe alone (`[steps(2), linear]`, giving `0 · 0 · 0.5 · 0.5 · 1`). Applying the property to
//! the effect as well would compose it with itself and be visibly wrong.

use deno_core::v8;

use page::animation::{compute_timing, interpolate, Easing, Phase, PlayState, Timing};
use page::style::declarative::{AnimatedValue, CssAnimationSpec};
use page::style::property::PropertyId;
use page::PageHandle;

use crate::binding::HostDom;

use super::animation::make_animation;
use super::keyframe_effect::make_effect;
use super::{with_store, Keyframe, PROTO_CSS_ANIMATION, PROTO_CSS_TRANSITION};

/// What created an animation. The store holds all three kinds in one map.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Origin {
    /// `new Animation(...)` or `element.animate(...)`.
    Script,
    /// One entry of an element's `animation-name` list. `(node, index)` is the identity: CSS
    /// addresses an element's animations by their position in that list.
    Css { node: u32, index: usize, name: String },
    /// One property of one element, transitioning.
    Transition { node: u32, property: PropertyId },
}

impl Default for Origin {
    fn default() -> Self {
        Origin::Script
    }
}

impl Origin {
    /// The composite-order rung. Measured in Chrome 150 on a page carrying one of each: the list
    /// came back `CSSTransition · CSSAnimation · CSSAnimation · Animation`.
    pub(crate) fn rung(&self) -> u8 {
        match self {
            Origin::Transition { .. } => 0,
            Origin::Css { .. } => 1,
            Origin::Script => 2,
        }
    }

    /// Within a rung: an element's animations are ordered by document position and then by their
    /// index in the `animation-name` list.
    pub(crate) fn within(&self) -> (u32, usize) {
        match self {
            Origin::Script => (0, 0),
            Origin::Css { node, index, .. } => (*node, *index),
            Origin::Transition { node, .. } => (*node, 0),
        }
    }
}

/// The effect end an animation's playback state is measured against.
fn timing_end(timing: &Timing) -> f64 {
    timing.end_time()
}

/// `"background-color"` -> `"backgroundColor"`, the name `getKeyframes()` reports.
fn idl_name(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut upper = false;
    for c in css.chars() {
        if c == '-' {
            upper = true;
            continue;
        }
        if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Ask the page what the stylesheets declare.
///
/// The whole scan happens inside the `OpState` borrow because `Dom` is not `Clone` and must not be
/// held across a V8 call — and it does not need to be: `Page::declarative_update` is pure Rust over
/// the DOM and the sheets, and hands back an owned answer.
fn declared(
    scope: &mut v8::PinScope,
) -> Option<(std::sync::Arc<page::Page>, page::style::declarative::DeclarativeUpdate)> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let mut st = op_state.borrow_mut();
    let media = crate::core::dom::media_context(&mut st);
    let page = st.try_borrow::<spec::kernel::Context>()?.get::<PageHandle>()?.0.clone();
    // A page with `@container` rules settles its container sizes with a whole layout before its
    // animations' values are worked out, and that layout kept no earlier one to carry from -- so lay the page out
    // where its geometry reads keep their layout, carrying what did not change over (`BoxTree::carry_from`), and the
    // settle finds the sizes current. A font probe on www.wayfair.com's search page cost one such layout per read.
    if st.try_borrow::<HostDom>().is_some_and(|d| page.containers_stale(&d.0, &media)) {
        crate::core::dom::geometry_queries::refresh_layout_cache(&mut st);
    }
    let dom = st.try_borrow::<HostDom>()?;
    let update = page.declarative_update(&dom.0, &media);
    Some((page, update))
}

/// The properties a SCRIPT animation holds at this tick's moment, handed to the page before its scan for transitions:
/// Blink starts no transition on a property an animation is interpolating
/// (`CanCalculateTransitionUpdateForProperty`). The page already knows the ones the last tick put on the cascade (CSS
/// animations and script ones); this adds a script animation played or made since then -- WPT
/// `style-change-events.html`'s covering animation, played after `style.opacity` changed and before the next style
/// change. "Holds" is `apply`'s test: not idle, and in effect at the moment.
///
/// Free when the document has no script animation: no clock read and no page lock (this runs once per
/// `getComputedStyle`). Returns the moment it read, when it read one, for the tick to keep.
fn transition_blockers(scope: &mut v8::PinScope) -> Option<Option<f64>> {
    // NOTHING WRITTEN SINCE THE LAST LOOK: every script animation running then is in the page's animated values from
    // that tick, which the scan reads too (`held_by_animation`), so the page needs no set -- and a document whose
    // animations only run pays no clock read, no set and no lock per call (ac's r19t perf: +4 us/call otherwise).
    let (writes, seen, on) = with_store(scope, |s| (s.writes(), s.blockers_writes.get(), s.blockers_on.get()));
    if writes == seen {
        if on {
            set_page_blockers(scope, std::collections::HashSet::new());
        }
        return None;
    }
    with_store(scope, |s| s.blockers_writes.set(writes));
    let any = with_store(scope, |s| {
        let anims = s.anims.borrow();
        s.order.borrow().iter().any(|h| anims.get(h).is_some_and(|a| !a.cancelled && matches!(a.origin, Origin::Script)))
    });
    if !any {
        if on {
            set_page_blockers(scope, std::collections::HashSet::new());
        }
        return None;
    }
    let now = super::timeline_now(scope);
    let held: std::collections::HashSet<(u32, PropertyId)> = with_store(scope, |s| {
        let anims = s.anims.borrow();
        let effects = s.effects.borrow();
        let mut held = std::collections::HashSet::new();
        for handle in s.order.borrow().iter() {
            let Some(a) = anims.get(handle) else { continue };
            if a.cancelled || !matches!(a.origin, Origin::Script) {
                continue;
            }
            let Some(rec) = a.effect.and_then(|e| effects.get(&e)) else { continue };
            let Some(target) = rec.target else { continue };
            let tt = if a.has_timeline { now } else { None };
            if a.playback.play_state(tt, timing_end(&rec.timing)) == PlayState::Idle {
                continue;
            }
            if compute_timing(&rec.timing, a.playback.current_time(tt)).progress.is_none() {
                continue;
            }
            for f in &rec.keyframes {
                for (_, css, _) in &f.props {
                    if let Some(p) = PropertyId::from_name(css) {
                        held.insert((target, p));
                    }
                }
            }
        }
        held
    });
    if !held.is_empty() || on {
        set_page_blockers(scope, held);
    }
    Some(now)
}

/// Hand the page the set ([`transition_blockers`]) and note whether it is empty.
fn set_page_blockers(scope: &mut v8::PinScope, held: std::collections::HashSet<(u32, PropertyId)>) {
    let page = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<PageHandle>()).map(|h| h.0.clone())
    };
    if let Some(page) = page {
        let on = !held.is_empty();
        page.set_transition_blockers(held);
        with_store(scope, |s| s.blockers_on.set(on));
    }
}

/// Build the keyframes of a CSS animation from the `@keyframes` rule of `name`.
///
/// The animation's own `animation-timing-function` becomes each keyframe's easing, and a keyframe
/// block that declares `animation-timing-function` itself overrides it for that keyframe — which is
/// exactly what Chrome reports out of `getKeyframes()` (see the module header).
fn css_keyframes(page: &page::Page, name: &str, default_easing: &str, tree: Option<u32>) -> Option<Vec<Keyframe>> {
    let blocks = page.keyframes_in(name, tree)?;
    if blocks.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(blocks.len());
    for (offset, decls) in blocks {
        let easing = decls
            .iter()
            .find(|(p, _)| *p == PropertyId::AnimationTimingFunction)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| default_easing.to_string());
        let props: Vec<(String, String, String)> = decls
            .iter()
            // The eight `animation-*` longhands inside a keyframe are not animated properties, they
            // are the keyframe's own timing declaration. CSS says every `animation-*` is ignored
            // inside `@keyframes` except `animation-timing-function`, which is consumed above.
            .filter(|(p, _)| !p.name().starts_with("animation-"))
            .map(|(p, v)| (idl_name(p.name()), p.name().to_string(), v.clone()))
            .collect();
        out.push(Keyframe {
            offset: Some(offset as f64),
            computed_offset: offset as f64,
            easing,
            composite: "auto".to_string(),
            props,
        });
    }
    Some(out)
}

/// The two keyframes of a transition: the value the element is showing, and the one the cascade has
/// arrived at. Chrome reports exactly this pair out of a `CSSTransition`'s `getKeyframes()`.
fn transition_keyframes(property: PropertyId, from: &str, to: &str, easing: &str) -> Vec<Keyframe> {
    let one = |offset: f64, value: &str| Keyframe {
        offset: Some(offset),
        computed_offset: offset,
        easing: easing.to_string(),
        composite: "auto".to_string(),
        props: vec![(idl_name(property.name()), property.name().to_string(), value.to_string())],
    };
    vec![one(0.0, from), one(1.0, to)]
}

/// The value each property of `frames` has at overall progress `p`, with the per-keyframe easing
/// applied to the segment the progress falls in.
fn values_at(frames: &[Keyframe], p: f64) -> Vec<(String, String)> {
    let mut names: Vec<&str> = Vec::new();
    for f in frames {
        for (_, css, _) in &f.props {
            if !names.contains(&css.as_str()) {
                names.push(css);
            }
        }
    }
    let mut out = Vec::with_capacity(names.len());
    for css in names {
        let pts: Vec<(f64, &str, &str)> = frames
            .iter()
            .filter_map(|f| {
                f.props
                    .iter()
                    .find(|(_, c, _)| c == css)
                    .map(|(_, _, v)| (f.computed_offset, v.as_str(), f.easing.as_str()))
            })
            .collect();
        if pts.is_empty() {
            continue;
        }
        if p <= pts[0].0 {
            out.push((css.to_string(), pts[0].1.to_string()));
            continue;
        }
        let last = *pts.last().expect("non-empty");
        if p >= last.0 {
            out.push((css.to_string(), last.1.to_string()));
            continue;
        }
        let i = pts.iter().position(|(o, _, _)| *o > p).unwrap_or(pts.len() - 1);
        let (o0, v0, e0) = pts[i - 1];
        let (o1, v1, _) = pts[i];
        let span = o1 - o0;
        let raw = if span > 0.0 { (p - o0) / span } else { 0.0 };
        // The SEGMENT's easing is the starting keyframe's, which is where a CSS animation's
        // `animation-timing-function` lives.
        let t = Easing::parse(e0).map(|e| e.eval(raw)).unwrap_or(raw);
        out.push((css.to_string(), interpolate(v0, v1, t)));
    }
    out
}

/// The style flush a member that reads `node`'s style or layout runs first -- Blink's
/// `Document::UpdateStyleAndLayoutTreeForElement` / `UpdateStyleAndLayoutForNode`, whose style recalc is where a
/// CSS transition starts against the before-change style the previous recalc computed (a bot-detection service
/// D7). For a node in a document only: a detached one has no style to flush, and `getComputedStyle` ran the tick
/// on that condition before.
///
/// Measured on Chrome 150 (`fixtures/css-answers/transition-probe.js`): an element styled once and then given
/// `opacity: 0.3` under `transition: opacity 5s linear` transitions after a flush through `offsetHeight`,
/// `offsetWidth`, `offsetTop`, `clientWidth`, `scrollHeight`, `getBoundingClientRect`, `getClientRects`,
/// `scrollTop` or a computed property READ -- and not after `getComputedStyle(el)` that reads nothing, nor after
/// `innerHTML`. This engine ran the tick at the `getComputedStyle` call and nowhere else.
///
/// A FLUSH THAT WOULD CHANGE NOTHING IS SKIPPED. Every layout read on an unchanged document ran the whole tick --
/// the transition blockers, a media context, the container check, the cascade fingerprint, the animation list --
/// to find nothing to do: `scrollTop` in a loop, `offsetTop` per element, `getComputedStyle` per property, and the
/// observers' geometry key twice per scroll container per frame. A tick that STARTED and ENDED with the same
/// [`Quiet`] -- with the containers current -- left the store, the page's animation state and everything it reads
/// as they were; a later tick starting from that same value takes the same early exit (the blockers unchanged,
/// no container settle, no rescan, the tick memo holding or the empty path on an empty map) and changes nothing,
/// so it is not run. Anything that could make it do work moves the value: a DOM, sheet, viewport, container or
/// animated-value change ([`page::Page::layout_fingerprint`]), a navigation (the page, the arena), any write to an
/// animation (the store's write count), the script-animation blockers, and a new rendering opportunity
/// (`frame_time`, which `advance_frame_time` clears and the next clock read sets).
pub(crate) fn flush_style_for(scope: &mut v8::PinScope, node: u32) -> Flush {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let (rev, armed, last_rev) = {
        let st = op_state.borrow();
        let Some(dom) = st.try_borrow::<HostDom>().filter(|d| d.0.is_connected(node)) else { return Flush::Detached };
        let memo = st.try_borrow::<FlushMemo>();
        (dom.0.revision(), memo.and_then(|m| m.armed), memo.and_then(|m| m.last_rev))
    };
    // THE DOM MOVED SINCE THE LAST FLUSH: this one cannot be skipped (the revision is in the key), and a tick right
    // after a change is not worth arming on -- a script that writes and reads in turn would pay the key twice per
    // read for nothing (+5,200 instructions per write-and-`offsetTop` on rw.html, 2026-10-23). The next flush
    // with nothing moved in between arms instead.
    if armed.map(|q| q.rev) != Some(rev) && last_rev != Some(rev) {
        set_flush_memo(&op_state, None, rev);
        tick(scope);
        return Flush::Ticked;
    }
    // Once for both looks: nothing a tick does moves the persona, the window or a frame's viewport.
    let media = crate::core::dom::geometry_queries::media_context(&op_state.borrow());
    let before = quiet(scope, &media);
    if before.is_some() && armed == before {
        return Flush::Quiet;
    }
    tick(scope);
    let after = quiet(scope, &media);
    let settled = before.is_some() && before == after && {
        let st = op_state.borrow();
        let page = st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<PageHandle>());
        match (page, st.try_borrow::<HostDom>()) {
            (Some(p), Some(d)) => !p.0.containers_stale(&d.0, &media),
            _ => false,
        }
    };
    set_flush_memo(&op_state, if settled { after } else { None }, rev);
    Flush::Ticked
}

fn set_flush_memo(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, armed: Option<Quiet>, rev: u64) {
    let mut st = op_state.borrow_mut();
    match st.try_borrow_mut::<FlushMemo>() {
        Some(m) => {
            m.armed = armed;
            m.last_rev = Some(rev);
        }
        None => st.put(FlushMemo { armed, last_rev: Some(rev) }),
    }
}

/// What [`flush_style_for`] did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Flush {
    /// The node is in no document: there was no style to flush.
    Detached,
    /// The tick was skipped: it would have changed nothing.
    Quiet,
    /// The tick ran.
    Ticked,
}

/// Everything a tick's early exits read, in one value ([`flush_style_for`]): the page and arena it would read,
/// the layout fingerprint (DOM revision and length, sheets, viewport, container sizes, animated values), the
/// animation store's write count, the transition blockers' state, and the moment the tick would sample.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Quiet {
    page: usize,
    instance: u64,
    /// `Dom::revision`, which `fp` covers too: kept apart for the churn test in [`flush_style_for`].
    rev: u64,
    fp: u64,
    writes: u64,
    seen: u64,
    on: bool,
    time: Option<u64>,
}

/// The [`Quiet`] the last tick a flush ran started and ended with, when it changed nothing; and the DOM revision
/// the last flush saw.
struct FlushMemo {
    armed: Option<Quiet>,
    last_rev: Option<u64>,
}

fn quiet(scope: &mut v8::PinScope, media: &page::style::media::MediaContext) -> Option<Quiet> {
    let (page, instance, rev, fp) = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        let handle = st.try_borrow::<spec::kernel::Context>()?.get::<PageHandle>()?;
        let dom = &st.try_borrow::<HostDom>()?.0;
        (std::sync::Arc::as_ptr(&handle.0) as usize, dom.instance(), dom.revision(), handle.0.layout_fingerprint(dom, media))
    };
    let (writes, seen, on, time) = with_store(scope, |s| (s.writes(), s.blockers_writes.get(), s.blockers_on.get(), s.frame_time.get().map(f64::to_bits)));
    Some(Quiet { page, instance, rev, fp, writes, seen, on, time })
}

/// Reconcile the store against what the stylesheets declare, then push the values the live
/// animations are contributing back onto the cascade.
///
/// Cheap when nothing has changed: `Page::declarative_update` is gated on the cascade's own
/// fingerprint, so the steady state of an animation playing over an untouched document is one
/// integer compare plus the interpolation of the animations that exist.
pub(crate) fn tick(scope: &mut v8::PinScope) {
    // Before the cascade is scanned for transitions: what the script animations hold now, and the moment
    // that was read at, which is then the whole tick's.
    let held_at = transition_blockers(scope);
    let Some((page, update)) = declared(scope) else { return };
    // NOTHING DECLARED AND NOTHING RUNNING: every step below is then a no-op except
    // `apply`'s last line, which hands the page an empty map -- so hand it one, and do not read the
    // clock. This runs once per `getComputedStyle` call, and `timeline_now` is a V8 property lookup
    // plus a JS call to `performance.now` (~500 ns of the call's ~4,000 on the P9c fixture), paid by
    // every document with no animation at all. The steps it skips are exactly the ones that iterate
    // `update`'s three lists and the store's `order` (step 1, `apply`) -- all four empty here -- and
    // the `anims` scan in step 2, whose result is only consulted per entry of `update.animations`.
    if update.animations.is_empty()
        && update.transitions.is_empty()
        && update.cancelled.is_empty()
        && with_store(scope, |s| s.order.borrow().is_empty())
    {
        page.set_animated(std::collections::HashMap::new());
        return;
    }
    // ONE moment for the whole tick. This is what a frame is, and reading the clock again per
    // animation is what made a transition's first sample wrong: created at t, then sampled at
    // t + 0.1 ms, `#tr` read `0.25001666` where Chrome reads exactly `0.25` in the same task.
    let now = match held_at {
        Some(t) => t,
        None => super::timeline_now(scope),
    };
    // NOTHING MOVED SINCE THE LAST TICK: the same moment, the cascade not rescanned, and no animation
    // written since -- every step below would find what it found then and hand the page the same values. A
    // script that reads layout in a loop flushes style per read, and each read ran the whole tick again: on
    // www.wayfair.com's search page 556 000 ticks in 44 s, 40 000 of them in one 1.1 s hover (2026-10-13).
    if !update.rescanned && with_store(scope, |s| s.tick_memo.borrow().as_ref().is_some_and(|m| m.holds(&page, &update, now, s.writes()))) {
        return;
    }

    // ── 1. the CSS animations the document declares ────────────────────────────────────────────
    //
    // `(node, index)` is the identity. A record whose spec is gone is CANCELLED and dropped:
    // measured in Chrome, setting `style.animationName = "none"` takes the animation out of
    // `getAnimations()` and setting it back puts a new one in.
    // Decided in the store, against each record's origin in place: copying every origin out first
    // (a name per CSS animation) was an allocation per animation per `getComputedStyle`.
    let doomed: Vec<u32> = with_store(scope, |s| {
        let anims = s.anims.borrow();
        let order = s.order.borrow();
        order
            .iter()
            .filter(|h| match anims.get(h).map(|a| &a.origin) {
                Some(Origin::Css { node, index, name }) => {
                    update.rescanned
                        && !update.animations.iter().any(|s| s.node == *node && s.index == *index && s.name == *name)
                }
                Some(Origin::Transition { node, property }) => {
                    update.cancelled.contains(&(*node, *property))
                        || update.transitions.iter().any(|t| t.node == *node && t.property == *property)
                }
                Some(Origin::Script) | None => false,
            })
            .copied()
            .collect()
    });
    let mut frame_wanted = false;
    for handle in doomed {
        with_store(scope, |s| {
            if let Some(a) = s.anims.borrow_mut().get_mut(&handle) {
                // A record a frame has seen running is CANCELLED to the page: `transitioncancel` /
                // `animationcancel` at the next frame. Chrome 150: a transition whose target moved
                // mid-run, and `display: none` mid-run, each sent one.
                if let Some(e) = cancel_event(s, a, now) {
                    s.css_queue.borrow_mut().push(e);
                    frame_wanted = true;
                }
                a.cancelled = true;
                a.playback.cancel();
            }
            // Out of the ORDER as well as cancelled: `getAnimations()` walks the order, and a record
            // the document no longer declares must not be reachable from it even by identity.
            s.order.borrow_mut().retain(|h| *h != handle);
        });
    }
    for (node, property) in &update.cancelled {
        page.forget_transition(*node, *property);
    }

    // ── 2. create what is missing ──────────────────────────────────────────────────────────────
    // By `(node, index)`, once: a scan of the list per spec, and then of the store again in
    // `sync_play_state`, was quadratic in the animations, per `getComputedStyle`.
    // The FIRST record per key in the store's order, as the scan found it.
    // A record step 1 took out of the order is over: its spec coming back -- an element shown again after
    // `display: none`, `animation-name` set back -- is a NEW animation (Chrome 150 restarts it). One a
    // script cancelled stays in the order and is not made again while the rule still declares it.
    let have: std::collections::HashMap<(u32, usize), u32> = with_store(scope, |s| {
        let mut have = std::collections::HashMap::new();
        let live: std::collections::HashSet<u32> = s.order.borrow().iter().copied().collect();
        for (h, a) in s.anims.borrow().iter() {
            if let Origin::Css { node, index, .. } = &a.origin {
                if live.contains(h) {
                    have.entry((*node, *index)).or_insert(*h);
                }
            }
        }
        have
    });
    for spec in update.animations.iter() {
        if let Some(&handle) = have.get(&(spec.node, spec.index)) {
            sync_play_state(scope, handle, spec, now);
            continue;
        }
        create_css_animation(scope, &page, spec, now);
        frame_wanted = true;
    }
    // Made in PROPERTY-NAME order: two transitions of one element that start together are ordered by their
    // names in composite order (Blink's `CSSTransition::HasLowerCompositeOrdering`, after the element and the
    // transition generation), so `color` before `opacity` -- Chrome 150 sent `color`'s events first for
    // `transition: opacity 60ms, color 60ms`. Creation order is the tie-break `getAnimations()` and
    // `send_events` sort by.
    let mut starts: Vec<&page::style::declarative::TransitionStart> = update.transitions.iter().collect();
    starts.sort_by_key(|t| t.property.name());
    for start in starts {
        create_transition(scope, start, now);
        page.note_transition_target(start.node, start.property, start.to.clone());
        frame_wanted = true;
    }
    // A record that can send an event needs the frames that send them (`send_events`).
    if frame_wanted {
        wake_after(scope);
    }

    // ── 3. what every live animation is putting on the cascade ─────────────────────────────────
    apply(scope, &page, now);
    let memo = TickMemo { page: std::sync::Arc::downgrade(&page), animations: update.animations.clone(), now, writes: 0 };
    with_store(scope, |s| *s.tick_memo.borrow_mut() = Some(TickMemo { writes: s.writes(), ..memo }));
}

/// What a CSS tick worked its values out from ([`tick`]): the page, the cascade's animation list, the moment and
/// the store's write count after it. The page is held weakly and the list strongly, so neither address can come back
/// as another's while this is kept.
pub(crate) struct TickMemo {
    page: std::sync::Weak<page::Page>,
    animations: std::sync::Arc<Vec<page::style::declarative::CssAnimationSpec>>,
    now: Option<f64>,
    writes: u64,
}

impl TickMemo {
    fn holds(&self, page: &std::sync::Arc<page::Page>, update: &page::style::declarative::DeclarativeUpdate, now: Option<f64>, writes: u64) -> bool {
        std::ptr::eq(self.page.as_ptr(), std::sync::Arc::as_ptr(page))
            && std::sync::Arc::ptr_eq(&self.animations, &update.animations)
            && self.now.map(f64::to_bits) == now.map(f64::to_bits)
            && self.writes == writes
    }
}

// ── the events ───────────────────────────────────────────────────────────────────────────

/// What a CSS animation or transition last reported to the page, for the next frame to compare with.
#[derive(Default, Clone, Copy)]
pub(crate) struct EventState {
    /// Seen by a frame. A record made since the last frame is PENDING through its first: Chrome 150 runs that
    /// frame's rAF callbacks before the record's first event (`raf listener handler` for a `transitionrun`
    /// listener, a `ontransitionrun` handler and an rAF callback set up in one task), because a pending animation
    /// gets its start time at the frame's commit and is read at the frame after.
    seen: bool,
    /// The phase it was in; `None` for idle.
    phase: Option<Phase>,
    /// Its current iteration then.
    iteration: Option<f64>,
    /// The start time it was made with, while nothing has moved it: the record is PENDING until a frame sees it.
    created: Option<f64>,
}

impl EventState {
    /// Made by the stylesheet and not yet seen by a frame, with the start time it was made with: Chrome's pending
    /// animation, which the frame that sees it first starts (see [`send_events`]). Its VALUES are not held at 0
    /// until then, as Chrome's are: a page with nothing else to run gets no frame here (the events' clock keeps no
    /// loop alive), and a held animation would never move -- a fade-in left transparent for `fetch --wait`.
    fn pending(&self, a: &page::animation::Playback) -> bool {
        !self.seen && self.created.is_some() && a.start_time == self.created && a.hold_time.is_none()
    }
}

/// One event a frame sends.
pub(crate) struct QueuedEvent {
    node: u32,
    transition: bool,
    kind: &'static str,
    /// `propertyName` or `animationName`.
    name: String,
    /// `elapsedTime`, in seconds.
    elapsed: f64,
}

/// The events between two phases of one record, in the order they are sent: CSS Transitions 2 §6.1 and CSS
/// Animations 2 §4.2.1. `None` is idle; a transition that leaves the before or active phase for idle was
/// cancelled, which [`cancel_event`] sends where the cancel happens.
fn kinds(transition: bool, was: Option<Phase>, now: Option<Phase>, iterated: bool) -> &'static [&'static str] {
    use Phase::{Active, After, Before};
    if transition {
        match (was, now) {
            (None, Some(Before)) => &["transitionrun"],
            (None, Some(Active)) => &["transitionrun", "transitionstart"],
            (None, Some(After)) => &["transitionrun", "transitionstart", "transitionend"],
            (Some(Before), Some(Active)) | (Some(After), Some(Active)) => &["transitionstart"],
            (Some(Before), Some(After)) | (Some(After), Some(Before)) => &["transitionstart", "transitionend"],
            (Some(Active), Some(After)) | (Some(Active), Some(Before)) => &["transitionend"],
            _ => &[],
        }
    } else {
        match (was, now) {
            (None | Some(Before), Some(Active)) | (Some(After), Some(Active)) => &["animationstart"],
            (None | Some(Before), Some(After)) | (Some(After), Some(Before)) => &["animationstart", "animationend"],
            (Some(Active), Some(After)) | (Some(Active), Some(Before)) => &["animationend"],
            (Some(Active), Some(Active)) if iterated => &["animationiteration"],
            _ => &[],
        }
    }
}

/// `elapsedTime` of a `kind` event, in seconds: the interval's start for `*run` / `*start`, its end for `*end`,
/// the iterations done for `animationiteration`, and the active time for a cancel (the specs' "interval
/// start" / "interval end" in §6.1 and §4.2.1).
fn elapsed(kind: &str, timing: &Timing, iteration: Option<f64>, local: Option<f64>) -> f64 {
    let active = timing.active_duration();
    let start = (-timing.delay).min(active).max(0.0);
    let end = (timing.end_time() - timing.delay).min(active).max(0.0);
    let ms = match kind {
        "transitionend" | "animationend" => end,
        "animationiteration" => (iteration.unwrap_or(0.0) - timing.iteration_start) * timing.duration.unwrap_or(0.0),
        "transitioncancel" | "animationcancel" => local.map_or(0.0, |t| (t - timing.delay).clamp(0.0, active)),
        _ => start,
    };
    ms / 1000.0
}

/// The record's identity for its events: the element, transition or not, and the name.
fn event_target(a: &super::AnimRecord, rec: &super::EffectRecord) -> Option<(u32, bool, String)> {
    let node = rec.target?;
    match &a.origin {
        Origin::Transition { property, .. } => Some((node, true, property.name().to_string())),
        Origin::Css { name, .. } => Some((node, false, name.clone())),
        Origin::Script => None,
    }
}

/// The cancel event of a record cancelled now, when a frame saw it in its before or active phase.
fn cancel_event(s: &super::AnimationStore, a: &super::AnimRecord, now: Option<f64>) -> Option<QueuedEvent> {
    if !a.css_events.seen || !matches!(a.css_events.phase, Some(Phase::Before | Phase::Active)) {
        return None;
    }
    let effects = s.effects.borrow();
    let rec = effects.get(&a.effect?)?;
    let (node, transition, name) = event_target(a, rec)?;
    let local = a.playback.current_time(if a.has_timeline { now } else { None });
    let kind = if transition { "transitioncancel" } else { "animationcancel" };
    Some(QueuedEvent { node, transition, kind, name, elapsed: elapsed(kind, &rec.timing, None, local) })
}

/// HTML's "update animations and send events" for CSS animations and transitions, run by the
/// rendering frame BEFORE its rAF callbacks: each record compares its phase and iteration at the frame's time
/// with what the last frame saw, and the events between are sent in composite order -- transitions, then CSS
/// animations, each by element and then creation -- after the cancels a style change queued since. Each is a
/// trusted `TransitionEvent` / `AnimationEvent` at the element, bubbling and cancelable, `pseudoElement` "".
/// Chrome 150 (`fixtures/css-animation-events`): one transition sends run, start, end (`elapsedTime` 0, 0,
/// 0.06); three starting together send each one's run and start in composite order, then the ends; an
/// animation of two 60 ms iterations sends start 0, iteration 0.06, end 0.12. There were none at all.
///
/// Returns whether a record can still send one as time passes -- not yet seen, or running in its before or active
/// phase -- for the caller to keep the events' clock going ([`wake_after`]). A paused one cannot.
pub(crate) fn send_events(scope: &mut v8::PinScope) -> bool {
    let now = super::timeline_now(scope);
    let frame = crate::core::intersection_observer::rendering::frame_interval_ms(scope);
    let (events, live) = with_store(scope, |s| {
        let mut events = std::mem::take(&mut *s.css_queue.borrow_mut());
        let mut anims = s.anims.borrow_mut();
        let effects = s.effects.borrow();
        let mut order: Vec<(u8, (u32, usize), usize, u32)> = s
            .order
            .borrow()
            .iter()
            .enumerate()
            .filter_map(|(i, h)| {
                let a = anims.get(h)?;
                (!matches!(a.origin, Origin::Script)).then(|| (a.origin.rung(), a.origin.within(), i, *h))
            })
            .collect();
        order.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
        let mut live = false;
        for (_, _, _, h) in order {
            let Some(a) = anims.get_mut(&h) else { continue };
            let Some(rec) = a.effect.and_then(|e| effects.get(&e)) else { continue };
            let Some((node, transition, name)) = event_target(a, rec) else { continue };
            if !a.css_events.seen {
                // The frame that STARTS a pending record: Chrome gives a CSS animation or transition its start time
                // when the frame after its style change commits, and reads it from the frame after that -- so its
                // timeline begins here, not at the style change, and its first events come a frame later. Starting
                // it at the style change instead ran a 60 ms transition to its end before the frame that sends
                // `transitionrun` (one frame's run, start and end together where Chrome sends run and start, then end).
                // A value read before this frame has moved by the time since the style change; this frame takes it
                // back to the start, by at most that much -- and no later than Chrome's frame would have come when the
                // HOST ran no loop in between: a page's frames run while its loop does, so a record made before
                // the host stopped running it (`korvus serve` between two commands, an embedder that evaluates and
                // waits) is started a frame after the loop went quiet, not at the late frame after the gap, which set a
                // fade-in read as finished back to transparent. A loop that kept running leaves no gap: a frame late
                // because the machine is busy starts it as before.
                if a.css_events.pending(&a.playback) && now.is_some() && a.has_timeline {
                    a.playback.start_time = match (now, a.css_events.created, s.loop_gap.get()) {
                        (Some(n), Some(made), Some((stopped, resumed))) if made <= resumed && resumed - made.max(stopped) > frame => {
                            Some(n.min(made.max(stopped) + frame))
                        }
                        (n, _, _) => n,
                    };
                }
                a.css_events.seen = true;
                live = true;
                continue;
            }
            let tt = if a.has_timeline { now } else { None };
            let idle = a.cancelled || a.playback.play_state(tt, timing_end(&rec.timing)) == PlayState::Idle;
            let local = if idle { None } else { a.playback.current_time(tt) };
            let timing = compute_timing(&rec.timing, local);
            let phase = Some(timing.phase).filter(|p| !idle && *p != Phase::Idle);
            let was = a.css_events;
            for &kind in kinds(transition, was.phase, phase, was.iteration != timing.current_iteration) {
                let elapsed = elapsed(kind, &rec.timing, timing.current_iteration, local);
                events.push(QueuedEvent { node, transition, kind, name: name.clone(), elapsed });
            }
            a.css_events.phase = phase;
            a.css_events.iteration = timing.current_iteration;
            let running = tt.is_some() && a.playback.start_time.is_some();
            live |= running && matches!(phase, Some(Phase::Before | Phase::Active));
        }
        (events, live)
    });
    for e in &events {
        dispatch(scope, e);
    }
    live
}

/// The host runs the page's loop again (`Engine::run`, a pump): the gap since it stopped is kept for a pending
/// CSS animation's start ([`send_events`]).
pub fn loop_resumed(scope: &mut v8::PinScope) {
    let now = crate::core::timing::performance::now_ms(scope);
    with_store(scope, |s| s.loop_gap.set(s.loop_stopped.get().filter(|_| now.is_finite()).map(|stopped| (stopped, now))));
}

/// The host stops running the page's loop.
pub fn loop_stopped(scope: &mut v8::PinScope) {
    let now = crate::core::timing::performance::now_ms(scope);
    with_store(scope, |s| s.loop_stopped.set(now.is_finite().then_some(now)));
}

/// How long until the page's next CSS animation or transition event, with the frame that sends it, in ms: when a
/// host that runs no loop while the page has nothing else to do -- `korvus serve`'s settled page -- should wake it. A
/// record no frame has seen is due at once (the frame that starts it); a running one at the end of its delay, its next
/// iteration boundary or its end ([`next_boundary`]); a paused, finished, cancelled, reversed or script-made one never.
/// An infinite 1.4 s spinner wakes the page once an iteration, not every frame; a value read in between is worked out
/// at the read. `None` when nothing is to come.
pub fn next_event_in(scope: &mut v8::PinScope) -> Option<f64> {
    let now = crate::core::timing::performance::now_ms(scope);
    if !now.is_finite() {
        return None;
    }
    let frame = crate::core::intersection_observer::rendering::frame_interval_ms(scope);
    with_store(scope, |s| {
        let anims = s.anims.borrow();
        let effects = s.effects.borrow();
        let mut due: Option<f64> = None;
        for a in anims.values() {
            if matches!(a.origin, Origin::Script) || a.cancelled || a.css_paused || !a.has_timeline {
                continue;
            }
            let Some(rec) = a.effect.and_then(|e| effects.get(&e)) else { continue };
            // the records a frame sends events for, and no other: one it skips would read as due for ever
            if event_target(a, rec).is_none() {
                continue;
            }
            let wait = if !a.css_events.seen {
                Some(0.0)
            } else {
                // what a frame now would send, worked out as [`send_events`] works it out: a phase or iteration other
                // than the last one sent is owed at once (the run and start a record's second frame sends)
                let tt = Some(now);
                let idle = a.playback.play_state(tt, timing_end(&rec.timing)) == PlayState::Idle;
                let local = if idle { None } else { a.playback.current_time(tt) };
                let timing = compute_timing(&rec.timing, local);
                let phase = Some(timing.phase).filter(|p| !idle && *p != Phase::Idle);
                let rate = a.playback.playback_rate;
                if phase != a.css_events.phase || (phase.is_some() && timing.current_iteration != a.css_events.iteration) {
                    Some(0.0)
                } else {
                    match (a.playback.start_time, local) {
                        (Some(_), Some(t)) if rate > 0.0 => next_boundary(&rec.timing, t).map(|b| ((b - t) / rate).max(0.0)),
                        _ => None,
                    }
                }
            };
            if let Some(w) = wait {
                due = Some(due.map_or(w, |d| d.min(w)));
            }
        }
        due.map(|d| d + frame)
    })
}

/// The next effect local time after `t` at which an effect timed by `timing` changes phase or iteration: the end of its
/// delay, an iteration boundary, the end of its active interval. `None` past that.
fn next_boundary(timing: &page::animation::Timing, t: f64) -> Option<f64> {
    if t < timing.delay {
        return Some(timing.delay);
    }
    let active_end = timing.delay + timing.active_duration();
    if t >= active_end {
        return None;
    }
    let d = timing.duration.unwrap_or(0.0);
    if !(d > 0.0 && d.is_finite()) {
        return active_end.is_finite().then_some(active_end);
    }
    let next = timing.delay + (((t - timing.delay) / d).floor() + 1.0) * d;
    Some(next.min(active_end))
}

/// Ask for a rendering frame about one frame from now, for the CSS events -- through a clock that does
/// NOT keep the event loop alive: an unref'd sleep (the stylesheet loads' `op_css_frame`, `sheet_loads::frame_after`)
/// whose end asks for the frame. While the page has other work -- a timer, a fetch, an animation frame, a host's
/// pump -- the frames come and the events are sent, as Chrome sends them. With none, the loop goes idle as it did
/// before the events existed: frames kept alive by an animation alone would hold `Engine::run`, a driver's idle
/// check and `fetch --wait` open for as long as it runs, for ever under `animation-iteration-count: infinite`
/// (`css_anim_live` hung on a paused one). Idempotent while the clock is armed.
pub(crate) fn wake_after(scope: &mut v8::PinScope) {
    if with_store(scope, |s| s.css_clock.replace(true)) {
        return;
    }
    let Some(f) = crate::core::xmlhttprequest::xml_http_request::deno_op(scope, "op_css_frame") else {
        with_store(scope, |s| s.css_clock.set(false));
        return;
    };
    // a millisecond: the frame it asks for comes at the next vsync, as every frame does (`next_frame_ms`)
    let ms: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, 1).into();
    let undef: v8::Local<v8::Value> = v8::undefined(scope).into();
    let Some(p) = f.call(scope, undef, &[ms]).and_then(|v| v8::Local::<v8::Promise>::try_from(v).ok()) else {
        with_store(scope, |s| s.css_clock.set(false));
        return;
    };
    crate::core::workers::worker::unref(scope, p.into());
    let tick = crate::binding::protos::build_fn(scope, cb_css_clock, "", 0);
    p.then2(scope, tick, tick);
}

fn cb_css_clock(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    with_store(scope, |s| s.css_clock.set(false));
    crate::core::intersection_observer::rendering::request_frame(scope);
}

/// Fire one queued event at its element, trusted, through the realm's own constructor (a page's replacement
/// `TransitionEvent` never builds one) and the engine's own `dispatchEvent`.
fn dispatch(scope: &mut v8::PinScope, e: &QueuedEvent) {
    use crate::binding::protos::{set_prop, str_val};
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let target = crate::binding::protos::wrap_cached(scope, &op_state, e.node);
    // A dictionary with no prototype: the constructor's reads see these members and nothing a page put on
    // `Object.prototype`.
    let init = v8::Object::new(scope);
    let null: v8::Local<v8::Value> = v8::null(scope).into();
    init.set_prototype(scope, null);
    let yes: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    set_prop(scope, init, "bubbles", yes);
    set_prop(scope, init, "cancelable", yes);
    let name = str_val(scope, &e.name);
    set_prop(scope, init, if e.transition { "propertyName" } else { "animationName" }, name);
    let elapsed: v8::Local<v8::Value> = v8::Number::new(scope, e.elapsed).into();
    set_prop(scope, init, "elapsedTime", elapsed);
    let empty = str_val(scope, "");
    set_prop(scope, init, "pseudoElement", empty);
    let ty = str_val(scope, e.kind);
    let iface = if e.transition { "TransitionEvent" } else { "AnimationEvent" };
    let Some(ev) = crate::binding::protos::new_global(scope, iface, &[ty, init.into()]) else { return };
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    crate::binding::protos::engine_call(scope, target.into(), "dispatchEvent", &[ev.into()]);
}

/// Follow a CHANGE to `animation-play-state`, and only a change.
///
/// `animation-play-state: paused` on a rule the page toggles (the pause-on-hover pattern) has to
/// pause the animation; the same property read again, unchanged, must not resume one that script
/// paused. So the record remembers what was last DECLARED and acts on the difference.
fn sync_play_state(scope: &mut v8::PinScope, handle: u32, spec: &CssAnimationSpec, now: Option<f64>) {
    with_store(scope, |s| {
        let end = super::effect_end(s, handle);
        let mut anims = s.anims.borrow_mut();
        let Some(a) = anims.get_mut(&handle) else { return };
        if a.css_paused == spec.paused {
            return;
        }
        a.css_paused = spec.paused;
        let _ = if spec.paused { a.playback.pause(now, end) } else { a.playback.play(now, end) };
    });
}

/// Mint one `CSSAnimation` and its `KeyframeEffect`, wearing the registry's prototypes.
fn create_css_animation(scope: &mut v8::PinScope, page: &page::Page, spec: &CssAnimationSpec, now: Option<f64>) {
    // `animation-name` naming a rule that does not exist declares NO animation. Measured:
    // `animation: nosuchkeyframes 4s` leaves `getAnimations()` empty while the computed
    // `animation-name` still reads `nosuchkeyframes`.
    // the keyframes of the element's own tree (a shadow root's are its, not the document's)
    let tree = {
        let op_state = deno_core::JsRuntime::op_state_from(scope);
        let st = op_state.borrow();
        st.try_borrow::<HostDom>().map(|d| d.0.root_of(spec.node))
    };
    let Some(frames) = css_keyframes(page, &spec.name, &spec.timing.easing_text, tree) else { return };
    let Some(obj) = super::new_on_proto(scope, PROTO_CSS_ANIMATION) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let target = crate::binding::protos::wrap_cached(scope, &op_state, spec.node);
    // The effect's own easing is LINEAR; the `animation-timing-function` is on the keyframes.
    let timing = Timing { easing_text: "linear".to_string(), easing: Easing::Linear, ..spec.timing.clone() };
    let Some(effect) = make_effect(scope, None, Some(target), Some(spec.node), frames, timing) else { return };
    let Some(handle) = super::effect_handle(scope, effect.into()) else { return };
    super::document_timeline(scope);
    let Some((_, anim)) = make_animation(scope, Some(obj), Some(handle), true) else { return };
    let tt = now;
    with_store(scope, |s| {
        let end = super::effect_end(s, anim);
        if let Some(a) = s.anims.borrow_mut().get_mut(&anim) {
            a.origin = Origin::Css { node: spec.node, index: spec.index, name: spec.name.clone() };
            a.css_paused = spec.paused;
            let _ = a.playback.play(tt, end);
            a.css_events.created = a.playback.start_time;
            if spec.paused {
                // `animation-play-state: paused` reads `playState "paused"`, `startTime null`,
                // `currentTime 0` in Chrome — which is a pause taken at time zero, not a pause of
                // something already running.
                a.playback.set_current_time(tt, 0.0);
                let _ = a.playback.pause(tt, end);
            }
        }
    });
}

/// Mint one `CSSTransition` and its two-keyframe effect.
fn create_transition(scope: &mut v8::PinScope, start: &page::style::declarative::TransitionStart, now: Option<f64>) {
    let frames = transition_keyframes(start.property, &start.from, &start.to, &start.timing.easing_text);
    let Some(obj) = super::new_on_proto(scope, PROTO_CSS_TRANSITION) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let target = crate::binding::protos::wrap_cached(scope, &op_state, start.node);
    let timing = Timing { easing_text: "linear".to_string(), easing: Easing::Linear, ..start.timing.clone() };
    let Some(effect) = make_effect(scope, None, Some(target), Some(start.node), frames, timing) else { return };
    let Some(handle) = super::effect_handle(scope, effect.into()) else { return };
    super::document_timeline(scope);
    let Some((_, anim)) = make_animation(scope, Some(obj), Some(handle), true) else { return };
    let tt = now;
    with_store(scope, |s| {
        let end = super::effect_end(s, anim);
        if let Some(a) = s.anims.borrow_mut().get_mut(&anim) {
            a.origin = Origin::Transition { node: start.node, property: start.property };
            let _ = a.playback.play(tt, end);
            a.css_events.created = a.playback.start_time;
        }
    });
}

/// Is any animation -- a stylesheet's, a transition, a script's -- running on the document's timeline now? While one is,
/// a frame consumer (a plugin's frame tap, a CDP screencast) is handed a frame at every vsync, as Chrome's compositor
/// draws one (D77): with no consumer the page settles as before, its values worked out at the next read.
pub(crate) fn running(scope: &mut v8::PinScope) -> bool {
    let Some(now) = super::timeline_now(scope) else { return false };
    with_store(scope, |s| {
        let anims = s.anims.borrow();
        let effects = s.effects.borrow();
        anims.values().any(|a| {
            if a.cancelled || !a.has_timeline {
                return false;
            }
            let Some(rec) = a.effect.and_then(|e| effects.get(&e)) else { return false };
            a.playback.play_state(Some(now), timing_end(&rec.timing)) == PlayState::Running
        })
    })
}

/// Recompute what every live animation is contributing and hand the whole map to the page.
///
/// Whole-map: a finished animation with `fill: none` must stop contributing, and an incremental
/// update would leave its last value on the element for ever.
pub(crate) fn apply(scope: &mut v8::PinScope, page: &page::Page, now: Option<f64>) {
    // Read in the store, in place: copying each record's origin, timing and keyframes out first was
    // www.mapbox.com's largest allocation churn left in the tick, per animation per `getComputedStyle`.
    let out = with_store(scope, |s| {
        let anims = s.anims.borrow();
        let effects = s.effects.borrow();
        let mut out: std::collections::HashMap<u32, Vec<AnimatedValue>> = std::collections::HashMap::new();
        for handle in s.order.borrow().iter() {
            // Every animation is read at the TICK's moment, not at its own -- the same reason a browser
            // resolves a whole frame against one timestamp. Reading the clock per animation would let
            // two animations on one element disagree about what time it is.
            let Some(a) = anims.get(handle) else { continue };
            let Some(e) = a.effect else { continue };
            let Some(rec) = effects.get(&e) else { continue };
            let tt = if a.has_timeline { now } else { None };
            let end = timing_end(&rec.timing);
            let state = if a.cancelled { PlayState::Idle } else { a.playback.play_state(tt, end) };
            let Some(target) = rec.target else { continue };
            if state == PlayState::Idle {
                continue;
            }
            // No progress = the effect is not in effect: before its delay with no backwards fill, or
            // past its end with no forwards fill. That is the case `fill: none` exists for, and it is
            // why the map is rebuilt rather than patched.
            let Some(progress) = compute_timing(&rec.timing, a.playback.current_time(tt)).progress else { continue };
            let transition = matches!(a.origin, Origin::Transition { .. });
            let values = values_at(&rec.keyframes, progress);
            let slot = out.entry(target).or_default();
            for (css, value) in values {
                let Some(property) = PropertyId::from_name(&css) else { continue };
                // A later animation on the same property replaces an earlier one, which is the
                // `animation-name` list's own order.
                slot.retain(|v: &AnimatedValue| v.property != property);
                slot.push(AnimatedValue { property, value, transition });
            }
        }
        out
    });
    page.set_animated(out);
}

// ── the two registry rows ───────────────────────────────────────────────────────────────────────

/// `CSSAnimation.animationName` and `CSSTransition.transitionProperty`.
///
/// These two interfaces stay REGISTRY rows rather than becoming `interface!` tables: they sit in the
/// capture's own-global order, and a realm's `globalThis` insertion order is fingerprint surface
/// (the same reason `OffscreenCanvas` keeps its rows, `core/html/html_canvas_element.rs`). The
/// generator's per-member kit claim is how a registry row reaches a Rust body; this is the function
/// it names, and it is also where the two prototypes are registered so instances can be minted
/// without going through `globalThis.CSSAnimation`, which a page can overwrite.
pub fn install_css_animation(
    scope: &mut v8::PinScope,
    proto: v8::Local<v8::Object>,
    member: &'static crate::install::MemberDef,
) {
    match member.name {
        "animationName" => {
            super::row_get(scope, proto, member, cb_css_name_get);
        }
        "transitionProperty" => {
            super::row_get(scope, proto, member, cb_css_name_get);
        }
        other => debug_assert!(false, "install_css_animation: unexpected member {other}"),
    }
}

/// Both getters, told apart by the member name in the function's `data` — and by the ORIGIN of the
/// record behind the receiver, which is the brand check. A plain `Animation` is not a `CSSAnimation`
/// and Chrome throws `Illegal invocation` for it.
fn cb_css_name_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let want_name = crate::binding::protos::body_data(scope, &args)
        .to_rust_string_lossy(scope)
        .starts_with("animationName");
    let this: v8::Local<v8::Value> = args.this().into();
    let origin = super::anim_handle(scope, this)
        .and_then(|h| with_store(scope, |s| s.anims.borrow().get(&h).map(|a| a.origin.clone())));
    let answer = match (want_name, origin) {
        (true, Some(Origin::Css { name, .. })) => name,
        (false, Some(Origin::Transition { property, .. })) => property.name().to_string(),
        _ => {
            crate::binding::protos::throw_type_error(scope, "Illegal invocation");
            return;
        }
    };
    let s = crate::binding::protos::str_val(scope, &answer);
    rv.set(s);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idl_names_are_the_camel_case_of_the_css_ones() {
        assert_eq!(idl_name("opacity"), "opacity");
        assert_eq!(idl_name("background-color"), "backgroundColor");
        assert_eq!(idl_name("border-top-left-radius"), "borderTopLeftRadius");
    }

    /// The per-keyframe easing is what a CSS animation's `animation-timing-function` becomes, and it
    /// applies to the SEGMENT. Chrome 150, `animation: three 4s linear` over
    /// `0% {opacity:0} 50% {opacity:0.2} 100% {opacity:1}`, sampled at progress 0/.25/.5/.75:
    /// `0 · 0.1 · 0.2 · 0.6`.
    #[test]
    fn a_segment_is_eased_by_its_starting_keyframe() {
        let kf = |o: f64, v: &str, e: &str| Keyframe {
            offset: Some(o),
            computed_offset: o,
            easing: e.to_string(),
            composite: "auto".into(),
            props: vec![("opacity".into(), "opacity".into(), v.into())],
        };
        let frames = vec![kf(0.0, "0", "linear"), kf(0.5, "0.2", "linear"), kf(1.0, "1", "linear")];
        let at = |p: f64| values_at(&frames, p)[0].1.clone();
        assert_eq!(at(0.0), "0");
        assert_eq!(at(0.25), "0.1");
        assert_eq!(at(0.5), "0.2");
        assert_eq!(at(0.75), "0.6");
        assert_eq!(at(1.0), "1");
    }

    /// …and a keyframe that declares its own easing overrides it for its own segment. Chrome 150,
    /// `from { opacity: 0; animation-timing-function: steps(2) } to { opacity: 1 }` with
    /// `animation: … 4s linear`, sampled at 0/.25/.5/.75/1: `0 · 0 · 0.5 · 0.5 · 1`.
    #[test]
    fn a_keyframes_own_timing_function_wins_for_its_own_segment() {
        let frames = vec![
            Keyframe {
                offset: Some(0.0),
                computed_offset: 0.0,
                easing: "steps(2)".into(),
                composite: "auto".into(),
                props: vec![("opacity".into(), "opacity".into(), "0".into())],
            },
            Keyframe {
                offset: Some(1.0),
                computed_offset: 1.0,
                easing: "linear".into(),
                composite: "auto".into(),
                props: vec![("opacity".into(), "opacity".into(), "1".into())],
            },
        ];
        let at = |p: f64| values_at(&frames, p)[0].1.clone();
        assert_eq!(at(0.0), "0");
        assert_eq!(at(0.25), "0");
        assert_eq!(at(0.5), "0.5");
        assert_eq!(at(0.75), "0.5");
        assert_eq!(at(1.0), "1");
    }

    #[test]
    fn composite_order_is_transitions_then_css_animations_then_script() {
        let mut rungs = [
            Origin::Script,
            Origin::Css { node: 1, index: 0, name: "a".into() },
            Origin::Transition { node: 1, property: PropertyId::Opacity },
        ];
        rungs.sort_by_key(|o| o.rung());
        assert_eq!(rungs[0].rung(), 0);
        assert!(matches!(rungs[0], Origin::Transition { .. }));
        assert!(matches!(rungs[1], Origin::Css { .. }));
        assert!(matches!(rungs[2], Origin::Script));
    }
}
