//! A stylesheet's animations, end to end in a live engine: they are in
//! `document.getAnimations()` wearing `CSSAnimation`/`CSSTransition`, **and the properties they
//! animate actually change value**.
//!
//! # Why both halves, always
//!
//! The interactivity axis scores `animation` when `document.getAnimations()` is non-empty **or** a
//! sampled `transform`/`opacity` moved. An engine can satisfy the first half by putting objects in
//! the list whose effects change nothing — and this engine already did: earlier,
//! `element.animate([{opacity:1},{opacity:0}], {duration:4000, fill:"both"})` seeked to
//! 0/1000/2000/3000/4000 read `opacity: "1"` at **every one of the five**. The list was true and the
//! element never moved. So every assertion here that an animation exists is paired with an
//! assertion that a computed value differs between two points on its own timeline.
//!
//! # There is no clock in this file, and that is deliberate
//!
//! Two gate tests in this tree asserted that no measurable wall time had passed and both went red
//! under load with correct engine behaviour. Nothing here sleeps or waits for a frame: every sample
//! is taken by **pausing the animation and assigning `currentTime`**, which is the same move that
//! produced the oracle. A machine under load reads exactly the same numbers.
//!
//! # The oracle
//!
//! Every expected value below was recorded from **stock Chrome 150.0.7871.46**, driven over CDP
//! against [`PAGE`] served from a loopback server, by pausing each animation and seeking it. Not one
//! number is from a specification. Chrome's answers, verbatim:
//!
//! ```text
//! document.getAnimations()
//!     CSSAnimation  #anim  slide
//!     CSSAnimation  #two   slide
//!     CSSAnimation  #two   shift        (#miss is ABSENT: its @keyframes does not exist)
//!
//! #anim, paused and seeked        0 -> opacity 0    width 10px
//!                              1000 -> opacity 0.25 width 35px
//!                              2000 -> opacity 0.5  width 60px
//!                              3000 -> opacity 0.75 width 85px
//!
//! #two (slide 8s + shift 2s)      0 -> opacity 0     width 10px    margin-left 0px
//!                              1000 -> opacity 0.125 width 22.5px  margin-left 20px
//!                              2000 -> opacity 0.25  width 35px    margin-left 0px
//!                                      (shift has run out and does not fill)
//!
//! classList.add("on") on #tr:  getComputedStyle(#tr).opacity is STILL "0.25" in the same task
//!     CSSTransition #tr opacity, duration 3000, fill "backwards",
//!     keyframes [{offset 0, opacity "0.25"}, {offset 1, opacity "0.75"}]
//!     paused and seeked   0 -> 0.25   750 -> 0.375   1500 -> 0.5   2250 -> 0.625   3000 -> 0.75
//!
//! CSSAnimation.prototype's animationName getter, called on {} / null / a plain Animation
//!     -> TypeError: Illegal invocation   (all three)
//! CSSTransition.prototype's transitionProperty getter on a CSSAnimation -> the same
//! ```

use engine::Engine;

/// The fixture Chrome answered on, byte for byte.
///
/// `#miss` names a `@keyframes` rule that does not exist, which is the case that separates "read
/// the `animation-name` longhand" from "find the rule it names".
const PAGE: &str = r#"<!doctype html><html><head><style>
  body { margin: 0 }
  @keyframes slide { from { opacity: 0; width: 10px } to { opacity: 1; width: 110px } }
  @keyframes shift { from { margin-left: 0px } to { margin-left: 40px } }
  #anim { animation: slide 4s linear; height: 10px }
  #tr   { transition: opacity 3s linear; opacity: 0.25; width: 30px; height: 10px }
  #tr.on { opacity: 0.75 }
  #two  { animation: slide 8s linear, shift 2s linear; height: 10px }
  #miss { animation: nosuchkeyframes 4s linear; width: 7px; height: 10px }
</style></head><body>
  <div id="anim"></div><div id="tr"></div><div id="two"></div><div id="miss"></div>
</body></html>"#;

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_html(PAGE).expect("load page");
    e.run().await.expect("run page");
    e.take_output();
    e
}

/// Evaluate `expr` and return the single line it printed.
async fn ask(e: &mut Engine, expr: &str) -> String {
    e.eval("p5b-ask", format!("print(String({expr}))")).expect("eval");
    e.run().await.expect("run");
    let out = e.take_output();
    assert_eq!(out.len(), 1, "one line expected from {expr:?}, got {out:?}");
    out.into_iter().next().unwrap()
}

/// Run `src` for its effect.
async fn run(e: &mut Engine, src: &str) {
    e.eval("p5b-run", src.to_string()).expect("eval");
    e.run().await.expect("run");
    e.take_output();
}

/// Pause the animation `pick` selects and read `props` at each of `times`, one row per time.
///
/// THE DETERMINISM: the animation is paused first, so `currentTime` is a hold time the assignment
/// sets exactly. No frame, no sleep, no tolerance.
async fn seek(e: &mut Engine, pick: &str, el: &str, props: &[&str], times: &[i64]) -> Vec<String> {
    run(e, &format!("globalThis.__a = ({pick}); globalThis.__a.pause();")).await;
    let mut rows = Vec::new();
    for t in times {
        let reads: Vec<String> =
            props.iter().map(|p| format!("s.getPropertyValue({p:?})")).collect();
        rows.push(
            ask(
                e,
                &format!(
                    "(() => {{ globalThis.__a.currentTime = {t}; \
                       const s = getComputedStyle(document.getElementById({el:?})); \
                       return [{}].join(' '); }})()",
                    reads.join(", ")
                ),
            )
            .await,
        );
    }
    rows
}

#[tokio::test]
async fn a_stylesheets_animations_are_in_the_list_wearing_their_own_interface() {
    let mut e = page().await;
    let list = ask(
        &mut e,
        r#"document.getAnimations().map(a =>
             a.constructor.name + " " + (a.effect.target.id || "?") + " " + a.animationName).join(" | ")"#,
    )
    .await;
    assert_eq!(
        list, "CSSAnimation anim slide | CSSAnimation two slide | CSSAnimation two shift",
        "composite order is document order, then the animation-name list's own order"
    );
}

/// `animation-name` naming a rule that does not exist declares NO animation, while the longhand
/// still reads back. That is the pair Chrome reports, and it is what says the engine looks the rule
/// up rather than believing the property.
#[tokio::test]
async fn a_name_with_no_keyframes_rule_declares_no_animation() {
    let mut e = page().await;
    assert_eq!(
        ask(&mut e, r#"document.getAnimations().some(a => a.effect.target.id === "miss")"#).await,
        "false"
    );
    assert_eq!(
        ask(&mut e, r#"getComputedStyle(document.getElementById("miss")).animationName"#).await,
        "nosuchkeyframes"
    );
    assert_eq!(
        ask(&mut e, r#"getComputedStyle(document.getElementById("miss")).width"#).await,
        "7px",
        "and nothing animated it"
    );
}

/// THE HALF THAT CANNOT BE FAKED. Four points on one animation's own timeline, four different
/// computed values, each the one Chrome reported.
#[tokio::test]
async fn a_css_animations_computed_value_differs_along_its_own_timeline() {
    let mut e = page().await;
    let rows = seek(
        &mut e,
        r#"document.getAnimations().find(a => a.effect.target.id === "anim")"#,
        "anim",
        &["opacity", "width"],
        &[0, 1000, 2000, 3000],
    )
    .await;
    assert_eq!(rows, vec!["0 10px", "0.25 35px", "0.5 60px", "0.75 85px"]);
    // Said as the capability says it, so the gate fails for the right reason if the rows above are
    // ever loosened: the value at one point differs from the value at another.
    assert_ne!(rows[0], rows[3], "the animated properties MOVED");
    assert_eq!(
        ask(&mut e, "globalThis.__a.playState").await,
        "paused",
        "and no wall-clock time was involved in saying so"
    );
}

/// Two animations on one element, with different durations, on different properties -- and the
/// shorter one stops contributing when it runs out, because `animation-fill-mode` is `none`.
///
/// `width` is NOT sampled here, and the reason is a defect this gate found that is not this lane's:
/// Chrome reads `22.5px` at t=1000 and this engine reads `23px`, because the used width comes back
/// from taffy's integer-rounded layout. It has nothing to do with animation -- a plain
/// `<div style="width: 22.5px">` reads `23px` here and `22.5px` in Chrome, and
/// `getBoundingClientRect().width` is `23` against `22.5` (measured, both engines, one fixture).
/// `core/korvus-page/src/layout/**` is P5a's, so the finding is REPORTED rather than fixed, and this
/// test samples the two properties whose values are exact on both engines. The whole-pixel widths in
/// `a_css_animations_computed_value_differs_along_its_own_timeline` are unaffected by it.
#[tokio::test]
async fn two_animations_on_one_element_each_run_on_their_own_duration() {
    let mut e = page().await;
    run(
        &mut e,
        r#"globalThis.__two = document.getAnimations().filter(a => a.effect.target.id === "two");
           globalThis.__two.forEach(a => a.pause());"#,
    )
    .await;
    let mut rows = Vec::new();
    for t in [0, 1000, 2000] {
        rows.push(
            ask(
                &mut e,
                &format!(
                    r#"(() => {{ globalThis.__two.forEach(a => {{ a.currentTime = {t}; }});
                        const s = getComputedStyle(document.getElementById("two"));
                        return [s.opacity, s.marginLeft].join(" "); }})()"#
                ),
            )
            .await,
        );
    }
    assert_eq!(
        rows,
        vec![
            "0 0px",
            "0.125 20px",
            // `shift` is 2s and does not fill, so at 2000 its margin-left is gone while `slide`,
            // which is 8s, is only a quarter of the way through its opacity.
            "0.25 0px",
        ]
    );
}

/// THE TRANSITION HALF. It exists as a `CSSTransition`, it carries the property it is transitioning,
/// and its computed value differs along its timeline -- and, in the same task as the class change,
/// the element still reads the OLD value, which is what says a transition started rather than the
/// cascade simply jumping.
#[tokio::test]
async fn a_css_transition_exists_and_its_computed_value_moves() {
    let mut e = page().await;
    assert_eq!(
        ask(&mut e, r#"getComputedStyle(document.getElementById("tr")).opacity"#).await,
        "0.25"
    );
    let immediate = ask(
        &mut e,
        r#"(() => { document.getElementById("tr").classList.add("on");
             return getComputedStyle(document.getElementById("tr")).opacity; })()"#,
    )
    .await;
    assert_eq!(immediate, "0.25", "the transition starts at the value the element was showing");

    let meta = ask(
        &mut e,
        r#"(() => { const t = document.getAnimations().find(a => a.constructor.name === "CSSTransition");
             return [t.constructor.name, t.transitionProperty, t.effect.target.id,
                     t.effect.getTiming().duration, t.effect.getTiming().fill,
                     JSON.stringify(t.effect.getKeyframes().map(k => [k.computedOffset, k.opacity]))
                    ].join(" "); })()"#,
    )
    .await;
    assert_eq!(
        meta,
        r#"CSSTransition opacity tr 3000 backwards [[0,"0.25"],[1,"0.75"]]"#,
        "Chrome reports fill `backwards` on a transition, and the two endpoint keyframes"
    );

    let rows = seek(
        &mut e,
        r#"document.getAnimations().find(a => a.constructor.name === "CSSTransition")"#,
        "tr",
        &["opacity"],
        &[0, 750, 1500, 2250, 3000],
    )
    .await;
    assert_eq!(rows, vec!["0.25", "0.375", "0.5", "0.625", "0.75"]);
    assert_ne!(rows[0], rows[2], "the transitioned property MOVED");
}

/// The two rows are branded on the RECORD's origin, not on a prototype walk: a plain `Animation` is
/// not a `CSSAnimation` and a `CSSAnimation` is not a `CSSTransition`. Chrome throws
/// `TypeError: Illegal invocation` for all five of these.
#[tokio::test]
async fn the_two_css_rows_refuse_a_foreign_receiver() {
    let mut e = page().await;
    let out = ask(
        &mut e,
        r#"(() => {
             const g = Object.getOwnPropertyDescriptor(CSSAnimation.prototype, "animationName").get;
             const h = Object.getOwnPropertyDescriptor(CSSTransition.prototype, "transitionProperty").get;
             const plain = document.getElementById("miss").animate([{opacity: 1}, {opacity: 0}], 1000);
             const css = document.getAnimations().find(a => a.constructor.name === "CSSAnimation");
             const t = (f, recv) => { try { f.call(recv); return "no throw"; }
                                      catch (err) { return err.name + ": " + err.message; } };
             const r = [t(g, {}), t(g, null), t(g, plain), t(h, plain), t(h, css)];
             plain.cancel();
             return r.join(" | "); })()"#,
    )
    .await;
    assert_eq!(
        out,
        "TypeError: Illegal invocation | TypeError: Illegal invocation | \
         TypeError: Illegal invocation | TypeError: Illegal invocation | TypeError: Illegal invocation"
    );
}

/// The regression this lane found on the way past: a SCRIPT animation's effect never reached a
/// computed value either. Same seek, same determinism.
#[tokio::test]
async fn a_script_animation_moves_a_computed_value_too() {
    let mut e = page().await;
    run(
        &mut e,
        r#"globalThis.__s = document.getElementById("miss")
             .animate([{opacity: 1}, {opacity: 0}], {duration: 4000, easing: "linear", fill: "both"});
           globalThis.__s.pause();"#,
    )
    .await;
    let mut rows = Vec::new();
    for t in [0, 1000, 2000, 3000, 4000] {
        rows.push(
            ask(
                &mut e,
                &format!(
                    r#"(() => {{ globalThis.__s.currentTime = {t};
                        return getComputedStyle(document.getElementById("miss")).opacity; }})()"#
                ),
            )
            .await,
        );
    }
    assert_eq!(rows, vec!["1", "0.75", "0.5", "0.25", "0"]);
}


/// WHAT IS IN THE LIST, AND WHEN. The rule the engine used to apply -- "a finished, non-filling
/// animation stays in until it is cancelled" -- is not Chrome's, and it over-reported
/// `getAnimations().length` for ever after every short animation on the page had finished. On an
/// axis scored partly by that length, that is over-reporting in the flattering direction.
///
/// Chrome 150, one `element.animate(…, {duration: 100, fill})` per fill mode, paused and seeked,
/// asking `document.getAnimations().includes(a)`:
///
/// ```text
///   fill        t=0    t=50   t=100  t=500   after cancel()
///   none        in     in     OUT    OUT     out
///   backwards   in     in     OUT    OUT     out
///   forwards    in     in     in     in      out
///   both        in     in     in     in      out
///   delayed 5000ms, fill none, seeked to 0 (its BEFORE phase): IN, progress null
/// ```
#[tokio::test]
async fn the_list_holds_an_animation_while_it_is_in_effect_or_still_to_come() {
    let mut e = page().await;
    let out = ask(
        &mut e,
        r#"(() => {
             const host = document.getElementById("miss");
             const rows = [];
             for (const fill of ["none", "backwards", "forwards", "both"]) {
               const a = host.animate([{opacity: 1}, {opacity: 0}],
                                      {duration: 100, fill, easing: "linear"});
               a.pause();
               const at = (t) => { a.currentTime = t;
                 return document.getAnimations().includes(a) ? "in" : "out"; };
               rows.push(fill + " " + [at(0), at(50), at(100), at(500)].join(" "));
               a.cancel();
               rows.push(fill + " cancelled " + (document.getAnimations().includes(a) ? "in" : "out"));
             }
             const d = host.animate([{opacity: 1}, {opacity: 0}],
                                    {duration: 100, delay: 5000, fill: "none"});
             d.pause(); d.currentTime = 0;
             rows.push("delayed " + (document.getAnimations().includes(d) ? "in" : "out") +
                       " progress=" + d.effect.getComputedTiming().progress);
             d.cancel();
             return rows.join(" | "); })()"#,
    )
    .await;
    assert_eq!(
        out,
        "none in in out out | none cancelled out | \
         backwards in in out out | backwards cancelled out | \
         forwards in in in in | forwards cancelled out | \
         both in in in in | both cancelled out | \
         delayed in progress=null"
    );
}

/// `animation-play-state` is a CHANGE, not a state: toggling it to `paused` pauses the animation
/// and toggling it back plays it, while a script `pause()` on the same animation is not undone by
/// the next tick reading a property that has not moved. Measured in Chrome 150: an element whose
/// rule carries `animation-play-state: paused` reads `playState "paused"`, `startTime null`,
/// `currentTime 0`, and its first keyframe's value.
#[tokio::test]
async fn animation_play_state_pauses_and_resumes_without_fighting_a_script_pause() {
    let mut e = page().await;
    // The engine must not undo a script pause on the next observation.
    run(
        &mut e,
        r#"globalThis.__a = document.getAnimations().find(a => a.effect.target.id === "anim");
           globalThis.__a.pause(); globalThis.__a.currentTime = 1000;"#,
    )
    .await;
    assert_eq!(
        ask(&mut e, r#"getComputedStyle(document.getElementById("anim")).opacity"#).await,
        "0.25"
    );
    assert_eq!(ask(&mut e, "globalThis.__a.playState").await, "paused", "a tick did not resume it");

    // …and the property, when it CHANGES, does move the animation -- at the next OBSERVATION, which
    // is this engine's whole rule for animations (there is no frame to hang it on), so the style
    // read below is what processes the change rather than being incidental to the assertion.
    let flip = |v: &str| {
        format!(
            r#"(() => {{ document.getElementById("anim").style.animationPlayState = "{v}";
                 getComputedStyle(document.getElementById("anim")).opacity;
                 return globalThis.__a.playState; }})()"#
        )
    };
    assert_eq!(ask(&mut e, &flip("paused")).await, "paused");
    assert_eq!(
        ask(&mut e, &flip("running")).await,
        "running",
        "the declaration moved from paused to running, so the animation plays"
    );
}

/// A DISCRETE property starts no transition unless its entry is `allow-discrete` (Blink leaves it out of
/// `transition: all` and finds no smooth interpolation when it is named). WPT `css-transitions/transition-behavior.html`,
/// which Chrome 150 passes, here with the page of its two subtests plus one element that changes an interpolable
/// property and a discrete one together:
///
/// ```text
/// transition: all 1s, class change to float: right                        0 animations
/// the same with transition-behavior: allow-discrete                        1, CSSTransition `float`
/// transition: all 1s, opacity 0.25 -> 0.75 and float: right together       1, `opacity` only
/// transition: all 1s normal, float 1s allow-discrete; style.float = right  1 (the later entry wins)
/// ```
///
/// korvus transitioned every property that changed, `float` included, so the first and third rows read 1 and 2. The
/// class change in WPT's first subtest started nothing at all in an older build, which is why that build passed it.
#[tokio::test]
async fn a_discrete_property_transitions_only_when_its_entry_allows_it() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_html(
        r#"<!doctype html><html><head><style>
  .t { float: left; width: 100px; height: 100px; transition: all 1s; }
  #discrete { transition-behavior: allow-discrete; }
  #op { opacity: 0.25; }
  .animated { float: right; }
  #op.animated { opacity: 0.75; }
</style></head><body>
  <div class="t" id="discrete"></div><div class="t" id="normal"></div><div class="t" id="op"></div>
  <div id="overlap" style="transition: all 1s normal, float 1s allow-discrete; float: left"></div>
</body></html>"#,
    )
    .expect("load page");
    e.run().await.expect("run page");
    e.take_output();
    let got = ask(
        &mut e,
        r#"(() => {
             const row = (id) => { const el = document.getElementById(id);
               return id + ' ' + el.getAnimations().length + ' ' + el.getAnimations().map(a => a.transitionProperty).join(','); };
             for (const id of ['discrete', 'normal', 'op', 'overlap']) getComputedStyle(document.getElementById(id)).float;
             document.getElementById('normal').classList.add('animated');
             document.getElementById('discrete').classList.add('animated');
             document.getElementById('op').classList.add('animated');
             document.getElementById('overlap').style.float = 'right';
             return ['normal', 'discrete', 'op', 'overlap'].map(row).join(' | '); })()"#,
    )
    .await;
    assert_eq!(got, "normal 0  | discrete 1 float | op 1 opacity | overlap 1 float");
}

/// No transition starts on a property an ANIMATION holds (Blink's `CanCalculateTransitionUpdateForProperty`:
/// the property is in this update's, or the previous update's, active interpolations for animations;
/// `kCSSTransitionBlockedByAnimation`). WPT `web-animations/interfaces/{DocumentTimeline,Animation}/style-change-events.html`,
/// which Chrome 150 passes, play a covering `opacity` animation after `style.opacity` changed and expect no
/// `transitionrun`. Here with that case (`a`), the control with no animation (`b`), and a running CSS animation on the
/// same property (`c`):
///
/// ```text
/// a  style.opacity = 0.5, then a covering script animation played   Animation       (no transition)
/// b  style.opacity = 0.5                                            CSSTransition
/// c  style.opacity = 0.5 under a running `animation: fade`          CSSAnimation    (no transition)
/// ```
///
/// korvus started the transition in `a` and `c` too. Both passed in an older build only because, there, the first
/// style change on an idle page started no transition at all.
#[tokio::test]
async fn a_property_an_animation_holds_starts_no_transition() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.load_html(
        r#"<!doctype html><html><head><style>
  @keyframes fade { from { opacity: 0 } to { opacity: 1 } }
  #a, #b, #c { width: 10px; height: 10px; transition: opacity 100s; }
  #c { animation: fade 100s linear; }
</style></head><body><div id="a"></div><div id="b"></div><div id="c"></div></body></html>"#,
    )
    .expect("load page");
    e.run().await.expect("run page");
    e.take_output();
    let got = ask(
        &mut e,
        r#"(() => {
             const [a, b, c] = ['a', 'b', 'c'].map((id) => document.getElementById(id));
             const cover = new Animation(new KeyframeEffect(a, { opacity: [0, 1] }, 100000));
             for (const el of [a, b, c]) getComputedStyle(el).opacity;
             a.style.opacity = '0.5';
             cover.play();
             b.style.opacity = '0.5';
             c.style.opacity = '0.5';
             for (const el of [a, b, c]) getComputedStyle(el).opacity;
             const kinds = (el) => el.getAnimations().map((x) => x.constructor.name).join(',');
             return ['a ' + kinds(a), 'b ' + kinds(b), 'c ' + kinds(c)].join(' | '); })()"#,
    )
    .await;
    assert_eq!(got, "a Animation | b CSSTransition | c CSSAnimation");
}
