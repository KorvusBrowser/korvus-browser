//! The CSS half of the animation model: what `animation-*` and `transition-*` DECLARE.
//!
//! # The split, restated
//!
//! `crate::animation` owns an animation's numbers; `core/korvus-webapi/src/core/animation/` owns
//! its identity — the `Animation` object, its playback, its promises. This file is the third piece:
//! it reads an element's computed `animation-*` and `transition-*` longhands and says **which
//! animations should exist**, and it notices when a transitionable property's cascaded value
//! changed and says **which transitions should start**. It creates nothing and plays nothing.
//!
//! That division is forced by where the facts live. Only this crate has the stylesheets, the
//! cascade and the `@keyframes`; only the V8 tier can own the object a page holds and calls
//! `pause()` on. So the engine's one animation store lives over there, and this file is the input
//! it reconciles against.
//!
//! # Why a transition is harder than an animation
//!
//! An animation is a function of the element's own computed style: read the eight longhands, and
//! the set of animations is determined. A transition is a function of a CHANGE — it starts when a
//! property's cascaded value moves, so something has to remember what that value was. That is the
//! `before` snapshot here: one `StyleKey` per element, from the previous pass.
//!
//! Measured on `/usr/bin/google-chrome` 150.0.7871.46, and both halves matter:
//!
//!   * An element that gets its `transition:` AND its new value in the SAME style change **does**
//!     transition. `#u { opacity: .25 }` + `#u.on { transition: opacity 3s linear; opacity: .75 }`,
//!     then `classList.add('on')`: opacity read `0.25` → `0.252775` → `0.319442`. So the
//!     transition properties are read from the AFTER-change style.
//!   * A freshly created element with `transition:` and a value in its very first style does
//!     **not** transition — `document.getAnimations()` stayed empty and the value was there at
//!     once. So an element with no before-change style starts nothing, which is why a node with no
//!     entry in `before` is skipped rather than compared against an initial value.

use std::collections::HashMap;

use korvus_dom::{Dom, NodeId};

use crate::animation::{Easing, FillMode, PlaybackDirection, Timing};
use crate::style::computed::{StyleArena, StyleKey};
use crate::style::property::{PropertyId, ALL};

/// One entry of an element's `animation-name` list, as the cascade declares it.
///
/// `(node, index)` is the identity: CSS Animations addresses an element's animations by their
/// position in the `animation-name` list, so `animation: a 1s, b 2s` becoming `animation: b 2s`
/// does not "rename" the first one, it cancels both and starts one.
#[derive(Debug, Clone, PartialEq)]
pub struct CssAnimationSpec {
    pub node: NodeId,
    pub index: usize,
    pub name: String,
    pub timing: Timing,
    /// `animation-play-state: paused`. Measured: such an animation reads `playState: "paused"`,
    /// `startTime: null`, `currentTime: 0`, and its first keyframe is applied.
    pub paused: bool,
}

/// A transition that has just become due.
#[derive(Debug, Clone, PartialEq)]
pub struct TransitionStart {
    pub node: NodeId,
    pub property: PropertyId,
    /// The value the element is showing right now — which, when a transition is interrupted
    /// mid-flight, is the interpolated value and not the old cascade value.
    pub from: String,
    /// The value the cascade has just arrived at.
    pub to: String,
    pub timing: Timing,
}

/// What one pass over the document found.
#[derive(Debug, Default, Clone)]
pub struct DeclarativeUpdate {
    /// Every CSS animation the document's current style declares, in document order and then in
    /// `animation-name` order. The caller reconciles its own records against this list. Shared: a
    /// skipped update -- one per `getComputedStyle` -- hands back the last rescan's list, and copying
    /// it, names and easings included, was www.mapbox.com's largest allocation churn after Rb.
    pub animations: std::sync::Arc<Vec<CssAnimationSpec>>,
    pub transitions: Vec<TransitionStart>,
    /// `(node, property)` whose running transition is no longer going anywhere the cascade agrees
    /// with, and must be cancelled.
    pub cancelled: Vec<(NodeId, PropertyId)>,
    /// False when the pass was skipped because nothing could have changed. The animation list is
    /// still the truth; `transitions` and `cancelled` are empty by construction.
    pub rescanned: bool,
}

/// One value a declarative animation is putting on an element.
pub use crate::style::compute::AnimatedValue;

/// The document's declarative-animation state: the before-change styles, and the values the
/// animation origins are currently contributing.
#[derive(Default, Clone)]
pub struct Declarative {
    /// The previous pass's computed style per node. `None` means "this element has not been styled
    /// before", which is what stops a fresh element from transitioning into its own first value.
    before: Vec<Option<StyleKey>>,
    /// What the caller last told us the animations are showing, per node.
    animated: HashMap<NodeId, Vec<AnimatedValue>>,
    /// The end value of each running transition, so a cascade that moves somewhere else can cancel
    /// it rather than fighting it.
    transition_targets: HashMap<(NodeId, PropertyId), String>,
    /// The fingerprint the `before` snapshot was taken at.
    fingerprint: Option<u64>,
    /// The properties a SCRIPT animation holds at the style change being scanned, put by the caller before
    /// its scan ([`crate::Page::set_transition_blockers`]): one played since the last tick is not in `animated` yet.
    blockers: std::collections::HashSet<(NodeId, PropertyId)>,
}

impl Declarative {
    /// The values to feed into `compute` for one element.
    pub fn animated_for(&self, node: NodeId) -> &[AnimatedValue] {
        self.animated.get(&node).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn is_empty(&self) -> bool {
        self.animated.is_empty()
    }

    /// A copy of the override map, for a caller that has to read it while `scan` holds `&mut self`.
    /// It is one entry per element with a live animation, which is a handful even on a busy page.
    pub fn animated_snapshot(&self) -> HashMap<NodeId, Vec<AnimatedValue>> {
        self.animated.clone()
    }

    /// The properties a script animation holds now (see `blockers`).
    pub fn set_blockers(&mut self, blockers: std::collections::HashSet<(NodeId, PropertyId)>) {
        self.blockers = blockers;
    }

    /// Whether an animation that is NOT a transition holds `property` of `node` -- one the last tick put on the
    /// cascade, or a script animation the caller says holds it now -- so no transition starts on it.
    /// Blink's `CanCalculateTransitionUpdateForProperty`: a property in this update's active interpolations for
    /// animations, or in the previous update's, starts no transition (`kCSSTransitionBlockedByAnimation`).
    fn held_by_animation(&self, node: NodeId, property: PropertyId) -> bool {
        self.blockers.contains(&(node, property))
            || self.animated.get(&node).is_some_and(|vs| vs.iter().any(|v| v.property == property && !v.transition))
    }

    /// Would `set_animated(values)` change anything a LAYOUT would notice?
    ///
    /// The caller's layout cache is keyed on a fingerprint carrying the animation revision, so a
    /// revision bump rebuilds the whole box tree. An animation over `opacity` or `transform` -- which
    /// is what the overwhelming majority of the web animates -- changes no box, and bumping for it
    /// would rebuild a 21,824-element tree on every `getComputedStyle`. Anything NOT known to be
    /// paint-only counts, so the conservative answer is the one a new property gets.
    pub fn layout_affecting_change(&self, values: &HashMap<NodeId, Vec<AnimatedValue>>) -> bool {
        let moved = |node: &NodeId, vs: &Vec<AnimatedValue>| -> bool {
            match self.animated.get(node) {
                None => vs.iter().any(|v| !paint_only(v.property)),
                Some(mine) => {
                    // A property that has STOPPED being animated is a change too.
                    mine.iter()
                        .filter(|a| !paint_only(a.property))
                        .any(|a| !vs.iter().any(|b| b.property == a.property && b.value == a.value))
                        || vs
                            .iter()
                            .filter(|b| !paint_only(b.property))
                            .any(|b| !mine.iter().any(|a| a.property == b.property && a.value == b.value))
                }
            }
        };
        values.iter().any(|(n, vs)| moved(n, vs))
            || self
                .animated
                .iter()
                .any(|(n, vs)| !values.contains_key(n) && vs.iter().any(|v| !paint_only(v.property)))
    }

    /// Would `set_animated(values)` change anything at all?
    pub fn animated_differs(&self, values: &HashMap<NodeId, Vec<AnimatedValue>>) -> bool {
        if self.animated.len() != values.len() {
            return true;
        }
        values.iter().any(|(node, vs)| match self.animated.get(node) {
            None => true,
            Some(mine) => {
                mine.len() != vs.len()
                    || mine.iter().zip(vs).any(|(a, b)| {
                        a.property != b.property || a.value != b.value || a.transition != b.transition
                    })
            }
        })
    }

    /// Replace the whole override map. The caller recomputes every live animation's values each
    /// tick, so a partial update would leave a finished animation's last value behind for ever.
    pub fn set_animated(&mut self, values: HashMap<NodeId, Vec<AnimatedValue>>) {
        self.animated = values;
    }

    /// The elements whose animated values differ between the current map and `values`: what
    /// a style pass has to cascade again when `values` replaces it. Compared as
    /// [`Declarative::animated_differs`] compares.
    pub fn moved(&self, values: &HashMap<NodeId, Vec<AnimatedValue>>) -> Vec<NodeId> {
        let same = |a: &[AnimatedValue], b: &[AnimatedValue]| {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.property == b.property && a.value == b.value && a.transition == b.transition)
        };
        let mut out: Vec<NodeId> = values
            .iter()
            .filter(|(n, vs)| !same(self.animated_for(**n), vs))
            .map(|(n, _)| *n)
            .collect();
        out.extend(self.animated.keys().filter(|n| !values.contains_key(n)));
        out
    }

    /// The elements that carry animated values now.
    pub fn animated_nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.animated.keys().copied()
    }

    pub fn note_transition_target(&mut self, node: NodeId, property: PropertyId, to: String) {
        self.transition_targets.insert((node, property), to);
    }

    pub fn forget_transition(&mut self, node: NodeId, property: PropertyId) {
        self.transition_targets.remove(&(node, property));
    }

    /// Drop everything. A navigation reuses node ids for different elements, so a stale
    /// before-change style would invent transitions out of the new document's first paint.
    pub fn clear(&mut self) {
        self.before.clear();
        self.animated.clear();
        self.transition_targets.clear();
        self.fingerprint = None;
    }
}

/// Can this property's value change without moving a single box?
///
/// The list is deliberately short and the default is "no": a property this does not name is assumed
/// to affect layout, so a new one is treated conservatively rather than silently skipping a relayout.
/// These are the ones the web actually animates, and none of them is read by the box tree.
fn paint_only(p: PropertyId) -> bool {
    matches!(
        p,
        PropertyId::Opacity
            | PropertyId::Transform
            | PropertyId::Filter
            | PropertyId::BackdropFilter
            | PropertyId::Color
            | PropertyId::BackgroundColor
            | PropertyId::BackgroundPosition
            | PropertyId::BackgroundPositionX
            | PropertyId::BackgroundPositionY
            | PropertyId::BackgroundImage
            | PropertyId::BoxShadow
            | PropertyId::TextShadow
            | PropertyId::BorderTopColor
            | PropertyId::BorderRightColor
            | PropertyId::BorderBottomColor
            | PropertyId::BorderLeftColor
            | PropertyId::OutlineColor
            | PropertyId::Fill
            | PropertyId::Stroke
            | PropertyId::ClipPath
            | PropertyId::MixBlendMode
    )
}

/// Is this one of the longhands whose presence makes an element worth styling for animation?
///
/// Thirteen: the eight `animation-*` the shorthand sets, plus the five `transition-*`. A rule that
/// declares none of them cannot give its element an animation or a transition, which is what lets
/// the document scan match every element but compute only a handful.
pub fn is_declarative(p: PropertyId) -> bool {
    matches!(
        p,
        PropertyId::AnimationName
            | PropertyId::AnimationDuration
            | PropertyId::AnimationDelay
            | PropertyId::AnimationTimingFunction
            | PropertyId::AnimationIterationCount
            | PropertyId::AnimationDirection
            | PropertyId::AnimationFillMode
            | PropertyId::AnimationPlayState
            | PropertyId::TransitionProperty
            | PropertyId::TransitionDuration
            | PropertyId::TransitionDelay
            | PropertyId::TransitionTimingFunction
            | PropertyId::TransitionBehavior
    )
}

/// The eight `animation-*` longhands of one element, split into one [`CssAnimationSpec`] per entry
/// of the `animation-name` list.
///
/// The lists are cycled, not zipped: `animation-name: a, b, c` with `animation-duration: 1s, 2s`
/// gives 1s, 2s, 1s. That is CSS's list-matching rule and Chrome's behaviour.
///
/// `get` lends the computed values ([`crate::style::computed::StyleArena::value`]), and nothing is copied
/// out of them but what a spec keeps. An element with no named animation -- nearly every element --
/// costs one read: a rescan asks this of every styled node, and copying eight values into lists for
/// each was the largest owner of www.mapbox.com's allocations.
pub fn animation_specs<'a>(node: NodeId, get: impl Fn(PropertyId) -> &'a str) -> Vec<CssAnimationSpec> {
    let names = comma_list(get(PropertyId::AnimationName));
    // `none` entries declare nothing (below): a list of only them is no animation at all.
    if names.iter().all(|n| n.eq_ignore_ascii_case("none")) {
        return Vec::new();
    }
    let durations = comma_list(get(PropertyId::AnimationDuration));
    let delays = comma_list(get(PropertyId::AnimationDelay));
    let easings = comma_list(get(PropertyId::AnimationTimingFunction));
    let counts = comma_list(get(PropertyId::AnimationIterationCount));
    let directions = comma_list(get(PropertyId::AnimationDirection));
    let fills = comma_list(get(PropertyId::AnimationFillMode));
    let states = comma_list(get(PropertyId::AnimationPlayState));

    let pick = |list: &[&'a str], i: usize, fallback: &'static str| -> &'a str {
        if list.is_empty() {
            fallback
        } else {
            list[i % list.len()]
        }
    };

    let mut out = Vec::new();
    for (i, name) in names.iter().enumerate() {
        // `animation-name: none` is an entry in the list that declares no animation. It still
        // occupies its index, which is why the loop does not filter the list before enumerating.
        if name.eq_ignore_ascii_case("none") {
            continue;
        }
        let easing_text = pick(&easings, i, "ease");
        let easing = Easing::parse(easing_text).unwrap_or(Easing::Linear);
        let iterations = {
            let c = pick(&counts, i, "1");
            if c.eq_ignore_ascii_case("infinite") {
                f64::INFINITY
            } else {
                c.parse::<f64>().ok().filter(|v| *v >= 0.0).unwrap_or(1.0)
            }
        };
        out.push(CssAnimationSpec {
            node,
            index: i,
            name: name.to_string(),
            timing: Timing {
                delay: parse_time_ms(pick(&delays, i, "0s")),
                end_delay: 0.0,
                // `animation-fill-mode` is where a CSS animation's fill comes from, so `auto` never
                // reaches it — the longhand's initial is `none`.
                fill: FillMode::parse(&pick(&fills, i, "none").to_ascii_lowercase()).unwrap_or(FillMode::None),
                iteration_start: 0.0,
                iterations,
                duration: Some(parse_time_ms(pick(&durations, i, "0s")).max(0.0)),
                direction: PlaybackDirection::parse(&pick(&directions, i, "normal").to_ascii_lowercase())
                    .unwrap_or(PlaybackDirection::Normal),
                easing_text: easing_text.to_string(),
                easing,
            },
            paused: pick(&states, i, "running").eq_ignore_ascii_case("paused"),
        });
    }
    out
}

/// The `transition-*` longhands of one element as `(property-or-all, timing, allow-discrete)` triples.
///
/// A LATER entry for the same property wins, which is what makes `transition: all 1s, opacity 3s`
/// give opacity three seconds. The third member is the entry's `transition-behavior`: `allow-discrete` lets
/// a discrete property transition ([`PropertyId::is_interpolable`]).
fn transition_specs<'a>(get: &impl Fn(PropertyId) -> &'a str) -> Vec<(String, Timing, bool)> {
    let props = comma_list(get(PropertyId::TransitionProperty));
    if props.is_empty() {
        return Vec::new();
    }
    let durations = comma_list(get(PropertyId::TransitionDuration));
    let delays = comma_list(get(PropertyId::TransitionDelay));
    let easings = comma_list(get(PropertyId::TransitionTimingFunction));
    let behaviors = comma_list(get(PropertyId::TransitionBehavior));
    let pick = |list: &[&'a str], i: usize, fallback: &'static str| -> &'a str {
        if list.is_empty() {
            fallback
        } else {
            list[i % list.len()]
        }
    };
    let mut out: Vec<(String, Timing, bool)> = Vec::new();
    for (i, p) in props.iter().enumerate() {
        if p.eq_ignore_ascii_case("none") {
            continue;
        }
        let easing_text = pick(&easings, i, "ease");
        let easing = Easing::parse(easing_text).unwrap_or(Easing::Linear);
        let timing = Timing {
            delay: parse_time_ms(pick(&delays, i, "0s")),
            end_delay: 0.0,
            // A transition's own timing reports `fill: "backwards"` in Chrome (measured off the
            // `CSSTransition`'s `effect.getTiming()`), which is what makes a delayed transition
            // hold the start value rather than jumping.
            fill: FillMode::Backwards,
            iteration_start: 0.0,
            iterations: 1.0,
            duration: Some(parse_time_ms(pick(&durations, i, "0s")).max(0.0)),
            direction: PlaybackDirection::Normal,
            easing_text: easing_text.to_string(),
            easing,
        };
        let discrete = pick(&behaviors, i, "normal").eq_ignore_ascii_case("allow-discrete");
        let key = p.to_ascii_lowercase();
        match out.iter_mut().find(|(n, _, _)| *n == key) {
            Some(slot) => {
                slot.1 = timing;
                slot.2 = discrete;
            }
            None => out.push((key, timing, discrete)),
        }
    }
    out
}

/// `"a, b"` -> `["a", "b"]`, stepping over parenthesised groups so a `cubic-bezier(.4, 0, .2, 1)`
/// stays one entry.
fn comma_list(value: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, c) in value.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(value[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = value[start..].trim();
    if !last.is_empty() {
        out.push(last);
    }
    out.retain(|s| !s.is_empty());
    out
}

/// `"4s"` / `"250ms"` / `"-1s"` -> milliseconds. An unparseable time is 0, which is the longhand's
/// initial value and therefore "no animation".
fn parse_time_ms(text: &str) -> f64 {
    let t = text.trim().to_ascii_lowercase();
    if let Some(n) = t.strip_suffix("ms") {
        return n.parse::<f64>().unwrap_or(0.0);
    }
    if let Some(n) = t.strip_suffix('s') {
        return n.parse::<f64>().unwrap_or(0.0) * 1000.0;
    }
    0.0
}

/// Walk the document's computed styles and report what the declarations ask for.
///
/// `keys` is this pass's style per node — the caller has just computed it, so this function never
/// styles anything itself. `displayed` answers "what is this element showing for this property
/// right now", which is the value an interrupted transition has to start from. `hidden` answers
/// whether an element is not rendered (see [`not_rendered`]).
pub fn scan(
    state: &mut Declarative,
    dom: &Dom,
    arena: &StyleArena,
    keys: &[Option<StyleKey>],
    hidden: impl Fn(NodeId) -> bool,
    displayed: impl Fn(NodeId, PropertyId, &str) -> String,
) -> DeclarativeUpdate {
    let mut out = DeclarativeUpdate { rescanned: true, ..Default::default() };
    let mut animations = Vec::new();
    state.before.resize(dom.len().max(keys.len()), None);
    for (node, key) in keys.iter().enumerate() {
        let node = node as NodeId;
        let Some(key) = *key else { continue };
        if hidden(node) {
            not_rendered(state, node, &mut out);
            continue;
        }
        animations.extend(animation_specs(node, |p| arena.value(key, p)));
        transitions_of(state, arena, node, key, &mut out, &displayed);
    }
    out.animations = std::sync::Arc::new(animations);
    out
}

/// An element that is not rendered -- `display: none` on it or on a flat-tree ancestor -- runs no CSS animation
/// and no transition, and keeps no before-change style. So hiding one mid-run cancels what it runs (its
/// animations by declaring none here, its transitions through `out.cancelled`), and showing it again starts no
/// transition into the values it has then. Chrome 150 (`fixtures/css-animation-events`): `display: none` sent
/// `transitioncancel` and `animationcancel`; an element shown with a new `opacity`, itself or through its parent,
/// sent nothing. Blink keeps no `ComputedStyle` for such an element, which is where its before-change style lives.
///
/// The cancelled transitions are forgotten here, not only by the caller's `forget_transition`: a scan that starts over
/// visits a hidden element again, and must not cancel what it cancelled already -- the incremental scan, which visits
/// it only when something moved, would not (`KORVUS_VERIFY_INCREMENTAL`'s random-mutation test).
fn not_rendered(state: &mut Declarative, node: NodeId, out: &mut DeclarativeUpdate) {
    state.before[node as usize] = None;
    let mut running: Vec<PropertyId> = state.transition_targets.keys().filter(|(n, _)| *n == node).map(|(_, p)| *p).collect();
    running.sort_by_key(|p| p.name());
    for p in running {
        state.transition_targets.remove(&(node, p));
        if !out.cancelled.contains(&(node, p)) {
            out.cancelled.push((node, p));
        }
    }
}

/// One candidate's turn in a scan: its before-change style becomes `key`, and if it had one that
/// differs, the transitions the change starts (and the running ones it cancels) go into `out`.
fn transitions_of(
    state: &mut Declarative,
    arena: &StyleArena,
    node: NodeId,
    key: StyleKey,
    out: &mut DeclarativeUpdate,
    displayed: &impl Fn(NodeId, PropertyId, &str) -> String,
) {
    let get = |p: PropertyId| arena.value(key, p);
    let previous = state.before[node as usize];
    state.before[node as usize] = Some(key);
    // No before-change style: a fresh element does not transition into its first value.
    let Some(previous) = previous else { return };
    // The style is interned, so an unchanged element is one integer comparison.
    if previous == key {
        return;
    }
    let specs = transition_specs(&get);
    let all = specs.iter().find(|(n, _, _)| n == "all").map(|(_, t, d)| (t.clone(), *d));
    // A running transition whose property the new style no longer lists is cancelled -- `transition: none` stops
    // it where it is and the property takes its new value (Blink `CSSAnimations::CalculateTransitionUpdate`: an
    // active transition not in `listed_properties`, with no `all` among them, gets `CancelTransition`). Chrome 150
    // (`fixtures/css-answers/transition-probe.js`): no animation left and the cascade's value read at once; here the
    // transition ran on.
    if all.is_none() {
        let mut unlisted: Vec<PropertyId> = state
            .transition_targets
            .keys()
            .filter(|(n, p)| *n == node && !specs.iter().any(|(name, _, _)| name == p.name()))
            .map(|(_, p)| *p)
            .collect();
        unlisted.sort_by_key(|p| p.name());
        for p in unlisted {
            if !out.cancelled.contains(&(node, p)) {
                out.cancelled.push((node, p));
            }
        }
    }
    if specs.is_empty() {
        return;
    }
    // NOTHING CAN START OR STOP: every entry takes no time -- `transition: all 0s`, every element's initial
    // value -- and none of this element's transitions is running. The walk below over all ~480 properties would only
    // compare values: it starts no transition of zero duration, and it cancels only a running one. It was run for every
    // element whose style changed.
    if specs.iter().all(|(_, t, _)| t.iteration_duration() <= 0.0) && !state.transition_targets.keys().any(|(n, _)| *n == node) {
        return;
    }
    for property in ALL.iter().copied() {
        let named = specs
            .iter()
            .find(|(n, _, _)| *n == property.name())
            .map(|(_, t, d)| (t.clone(), *d))
            .or_else(|| all.clone());
        let Some((timing, allow_discrete)) = named else { continue };
        // AN ANIMATION HOLDS IT: no transition is computed for it at all, before anything else, as in
        // Blink (`CanCalculateTransitionUpdateForProperty` returns first): a running transition is not cancelled from
        // here either. WPT `web-animations/interfaces/{DocumentTimeline,Animation}/style-change-events.html`, which
        // Chrome 150 passes: a covering `opacity` animation played after `style.opacity` changed makes no transition.
        if state.held_by_animation(node, property) {
            continue;
        }
        // compared as computed: `currentcolor` the keyword on both sides is no change, whatever `color` did
        if arena.computed_text(previous, property) == arena.computed_text(key, property) {
            continue;
        }
        // and run between the computed values: a styleless border's width as it computed, not the used `0px` (Chrome
        // 152, `vtr2.html`: `border: none` -> `border: 1px solid` runs `3px` -> `1px`), a shadow's `currentcolor` as
        // the element's `color`
        let (old, new) = (arena.transition_value(previous, property), arena.transition_value(key, property));
        let (old, new) = (old.as_ref(), new.as_ref());
        // A transition already heading somewhere else is over: the cascade has moved on.
        if let Some(target) = state.transition_targets.get(&(node, property)) {
            if target != new {
                out.cancelled.push((node, property));
            }
        }
        // A DISCRETE property starts no transition unless its entry allows it: Blink leaves it out of `all`
        // (`PropertiesForTransitionAll`) and finds no smooth interpolation when it is named (`discrete_interpolation`,
        // css_animations.cc). Chrome 150 (WPT `css-transitions/transition-behavior.html`): `transition: all 1s` and a
        // class change to `float: right` start nothing; `transition-behavior: allow-discrete` starts the `float` one.
        // AFTER the cancellation above, in Blink's order: a running transition the value has moved past is cancelled
        // either way, and only then is the new one not started (`UnstartTransition`).
        if !allow_discrete && !property.is_interpolable() {
            continue;
        }
        // Zero total time is not a transition, it is the value changing. Chrome creates no
        // `CSSTransition` for `transition-duration: 0s`, which is the longhand's initial and
        // therefore the overwhelmingly common case.
        if timing.iteration_duration() <= 0.0 {
            continue;
        }
        out.transitions.push(TransitionStart {
            node,
            property,
            from: displayed(node, property, old),
            to: new.to_string(),
            timing,
        });
    }
}

/// [`scan`] over the elements in `touched` only: `(node, Some(style), was scanned)` for a
/// node scanned now, `(node, None, _)` for one that is not, in node order. Every other scanned node is
/// one whose style did not change, which [`scan`] would pass over (no transition, the same specs), so
/// the answer is [`scan`]'s as long as `touched` holds every scanned node whose style changed and every
/// node that joined or left. `specs` keeps each scanned node's animation specs between calls; the list
/// handed back is theirs, in node order, as [`scan`]'s is. With it: how many nodes' specs were read.
pub fn scan_touched(
    state: &mut Declarative,
    dom: &Dom,
    arena: &StyleArena,
    touched: &[(NodeId, Option<StyleKey>, bool)],
    specs: &mut std::collections::BTreeMap<NodeId, Vec<CssAnimationSpec>>,
    hidden: impl Fn(NodeId) -> bool,
    displayed: impl Fn(NodeId, PropertyId, &str) -> String,
) -> (DeclarativeUpdate, usize) {
    let mut out = DeclarativeUpdate { rescanned: true, ..Default::default() };
    let mut read = 0;
    state.before.resize(dom.len().max(state.before.len()), None);
    for &(node, key, was_scanned) in touched {
        // An element this scan has no style for declares nothing and keeps what it had, as in `scan`. One that left
        // the flat tree did so by a slot assignment, after which the rescan starts over.
        let Some(key) = key else {
            specs.remove(&node);
            continue;
        };
        if hidden(node) {
            specs.remove(&node);
            not_rendered(state, node, &mut out);
            continue;
        }
        // Scanned last time with this style: the same specs, and no transition.
        if was_scanned && state.before[node as usize] == Some(key) {
            continue;
        }
        read += 1;
        let found = animation_specs(node, |p| arena.value(key, p));
        if found.is_empty() {
            specs.remove(&node);
        } else {
            specs.insert(node, found);
        }
        transitions_of(state, arena, node, key, &mut out, &displayed);
    }
    out.animations = std::sync::Arc::new(specs.values().flatten().cloned().collect());
    (out, read)
}

/// Where two rescans' answers differ, for `KORVUS_VERIFY_INCREMENTAL`: `(ours, our state)`
/// against `(theirs, their state)`. The animation list, the transitions started and cancelled, in
/// order, and each node's before-change style.
pub fn difference(ours: &DeclarativeUpdate, state: &Declarative, theirs: &DeclarativeUpdate, their_state: &Declarative) -> Option<String> {
    if ours.animations != theirs.animations {
        let first = ours.animations.iter().zip(theirs.animations.iter()).position(|(a, b)| a != b);
        return Some(format!(
            "{} animations, the whole rescan {}; first different: {:?} against {:?}",
            ours.animations.len(),
            theirs.animations.len(),
            first.map(|i| &ours.animations[i]),
            first.map(|i| &theirs.animations[i]),
        ));
    }
    if ours.transitions != theirs.transitions {
        return Some(format!("transitions {:?}, the whole rescan {:?}", ours.transitions, theirs.transitions));
    }
    if ours.cancelled != theirs.cancelled {
        return Some(format!("cancelled {:?}, the whole rescan {:?}", ours.cancelled, theirs.cancelled));
    }
    let n = state.before.len().max(their_state.before.len());
    let at = |v: &[Option<StyleKey>], i: usize| v.get(i).copied().flatten();
    (0..n)
        .find(|&i| at(&state.before, i) != at(&their_state.before, i))
        .map(|i| format!("node {i} before-change style {:?}, the whole rescan {:?}", at(&state.before, i), at(&their_state.before, i)))
}

pub fn needs_rescan(state: &Declarative, fingerprint: u64) -> bool {
    state.fingerprint != Some(fingerprint)
}

pub fn note_fingerprint(state: &mut Declarative, fingerprint: u64) {
    state.fingerprint = Some(fingerprint);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn getter<'a>(pairs: &'a [(PropertyId, &'a str)]) -> impl Fn(PropertyId) -> &'a str + 'a {
        move |p| pairs.iter().find(|(q, _)| *q == p).map(|(_, v)| *v).unwrap_or_else(|| p.initial())
    }

    #[test]
    fn one_animation_name_becomes_one_spec() {
        let specs = animation_specs(
            7,
            getter(&[
                (PropertyId::AnimationName, "slide"),
                (PropertyId::AnimationDuration, "4s"),
                (PropertyId::AnimationTimingFunction, "linear"),
            ]),
        );
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].node, 7);
        assert_eq!(specs[0].index, 0);
        assert_eq!(specs[0].name, "slide");
        assert_eq!(specs[0].timing.duration, Some(4000.0));
        assert_eq!(specs[0].timing.easing, Easing::Linear);
        assert_eq!(specs[0].timing.fill, FillMode::None);
        assert_eq!(specs[0].timing.iterations, 1.0);
        assert!(!specs[0].paused);
    }

    #[test]
    fn animation_name_none_declares_nothing() {
        assert!(animation_specs(1, getter(&[])).is_empty());
        assert!(animation_specs(1, getter(&[(PropertyId::AnimationDuration, "4s")])).is_empty());
    }

    #[test]
    fn the_lists_cycle_rather_than_zip() {
        // CSS list matching: three names against two durations gives 1s, 2s, 1s.
        let specs = animation_specs(
            1,
            getter(&[
                (PropertyId::AnimationName, "a, b, c"),
                (PropertyId::AnimationDuration, "1s, 2s"),
            ]),
        );
        assert_eq!(specs.len(), 3);
        assert_eq!(
            specs.iter().map(|s| s.timing.duration.unwrap()).collect::<Vec<_>>(),
            vec![1000.0, 2000.0, 1000.0]
        );
        assert_eq!(specs.iter().map(|s| s.index).collect::<Vec<_>>(), vec![0, 1, 2]);
    }

    #[test]
    fn every_longhand_reaches_the_timing() {
        let specs = animation_specs(
            1,
            getter(&[
                (PropertyId::AnimationName, "x"),
                (PropertyId::AnimationDuration, "2s"),
                (PropertyId::AnimationDelay, "-1s"),
                (PropertyId::AnimationIterationCount, "infinite"),
                (PropertyId::AnimationDirection, "alternate-reverse"),
                (PropertyId::AnimationFillMode, "both"),
                (PropertyId::AnimationPlayState, "paused"),
                (PropertyId::AnimationTimingFunction, "ease-in"),
            ]),
        );
        let s = &specs[0];
        assert_eq!(s.timing.delay, -1000.0);
        assert!(s.timing.iterations.is_infinite());
        assert_eq!(s.timing.direction, PlaybackDirection::AlternateReverse);
        assert_eq!(s.timing.fill, FillMode::Both);
        assert!(s.paused);
        assert_eq!(s.timing.easing_text, "ease-in");
        assert_eq!(s.timing.easing, Easing::parse("ease-in").unwrap());
    }

    #[test]
    fn a_millisecond_time_is_a_time() {
        assert_eq!(parse_time_ms("250ms"), 250.0);
        assert_eq!(parse_time_ms("0.5s"), 500.0);
        assert_eq!(parse_time_ms("-1s"), -1000.0);
        assert_eq!(parse_time_ms("nonsense"), 0.0);
    }

    #[test]
    fn a_comma_inside_a_function_is_not_a_separator() {
        assert_eq!(
            comma_list("cubic-bezier(0.1, 0.7, 1, 0.1), linear"),
            vec!["cubic-bezier(0.1, 0.7, 1, 0.1)", "linear"]
        );
    }

    #[test]
    fn a_later_transition_entry_for_the_same_property_wins() {
        let specs = transition_specs(&getter(&[
            (PropertyId::TransitionProperty, "all, opacity"),
            (PropertyId::TransitionDuration, "1s, 3s"),
        ]));
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].0, "all");
        assert_eq!(specs[0].1.duration, Some(1000.0));
        assert_eq!(specs[1].0, "opacity");
        assert_eq!(specs[1].1.duration, Some(3000.0));
    }

    #[test]
    fn each_entry_carries_its_own_transition_behavior() {
        // The WPT `transition-behavior.html` shorthand: `all 1s normal, float 1s allow-discrete`.
        let specs = transition_specs(&getter(&[
            (PropertyId::TransitionProperty, "all, float"),
            (PropertyId::TransitionDuration, "1s, 1s"),
            (PropertyId::TransitionBehavior, "normal, allow-discrete"),
        ]));
        assert_eq!(specs.iter().map(|(n, _, d)| (n.as_str(), *d)).collect::<Vec<_>>(), vec![("all", false), ("float", true)]);
        // the list cycles, as every `transition-*` list does
        let specs = transition_specs(&getter(&[
            (PropertyId::TransitionProperty, "float, display, opacity"),
            (PropertyId::TransitionDuration, "1s"),
            (PropertyId::TransitionBehavior, "allow-discrete, normal"),
        ]));
        assert_eq!(specs.iter().map(|(_, _, d)| *d).collect::<Vec<_>>(), vec![true, false, true]);
    }

    #[test]
    fn a_transitions_own_timing_fills_backwards() {
        // Read off `CSSTransition.effect.getTiming().fill` in Chrome 150: "backwards", not "none".
        let specs = transition_specs(&getter(&[
            (PropertyId::TransitionProperty, "opacity"),
            (PropertyId::TransitionDuration, "3s"),
        ]));
        assert_eq!(specs[0].1.fill, FillMode::Backwards);
    }
}
