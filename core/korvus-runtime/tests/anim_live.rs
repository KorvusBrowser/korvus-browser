//! LIVE one-table gate for the nine Web Animations interfaces.
//!
//! Boots the real engine and asserts, against the shipped capture bundle and against headless Chrome,
//! that the nine `interface!` tables reproduce -- byte- and value-identically -- what
//! `dom/09j-animation.js` installed. The bodies were ALREADY Rust before this lane, so the whole
//! question is whether moving the DEFINITION SITE changed an answer; these tests are what says it did
//! not.
//!
//!   1. **Byte-identical prototypes.** `describe_own(Iface.prototype)` is the bundle's member list for
//!      that interface, in the capture's order, with the capture's descriptors, `constructor` last
//!      (the hand-table shape every earlier lane landed on).
//!   2. **The interface objects and the chains**, including the three interfaces the REGISTRY still
//!      owns that inherit from two of these -- `ViewTimeline` from `ScrollTimeline`, `CSSAnimation`
//!      and `CSSTransition` from `Animation`. That is the install-ORDER gate: `install_window_animations`
//!      runs before `install_window_abort_style`, whose `rechain_orphans` sweep is what repairs those three,
//!      and moving it later breaks all three chains.
//!   3. **Every minted instance wears the TABLE's prototype** -- `element.animate()`'s `Animation`,
//!      its `KeyframeEffect`, `document.timeline`, and the `AnimationPlaybackEvent` a finishing
//!      animation dispatches. The four are minted through `core/animation/mod.rs`'s registered
//!      prototypes, which `install_window_animations` now fills from Rust instead of
//!      `op_anim_register_proto` from JS.
//!   4. **No row accepts a foreign receiver.** Recorded from `google-chrome --headless=new
//!      --dump-dom` (150.0.7871.46) over a loopback server: every accessor and method of all nine
//!      throws `TypeError: Illegal invocation` for a receiver that is not an instance of its
//!      interface. See `the_timeline_rows_refuse_a_foreign_receiver` for the one family where that
//!      was NOT true before this lane, and the note on the event slot getters.
//!   5. **The WebIDL arity guards** on the two rows that declare a required argument
//!      (`Animation.updatePlaybackRate`, `KeyframeEffect.setKeyframes`). The nine left the generated
//!      registry with the tables, so `finalize_surface`'s arity pass no longer reaches them -- the
//!      row's own guard, applied at install by `core::animation::row_method`, is the only thing
//!      enforcing it. (They left `generated_order.rs` too; that second table is deleted.)

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body><div id=box></div></body></html>";

/// The nine this lane's tables own.
const IFACES: &[&str] = &[
    "Animation",
    "AnimationEffect",
    "KeyframeEffect",
    "AnimationTimeline",
    "DocumentTimeline",
    "ScrollTimeline",
    "AnimationEvent",
    "TransitionEvent",
    "AnimationPlaybackEvent",
];

const DESCRIBE: &str = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
    const d = Object.getOwnPropertyDescriptor(o, n);
    return (d.get || d.set)
        ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
        : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
}).join("\n");"#;

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();
    e
}

fn bundle() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(persona::shipped_artifact("surface")).unwrap()).unwrap()
}

fn describe_line(m: &serde_json::Value) -> String {
    let n = m["name"].as_str().unwrap();
    match m["kind"].as_str().unwrap() {
        "accessor" => format!("{n}:accessor:e=1:c=1:s={}", m["set"].as_bool().unwrap() as u8),
        "method" => format!("{n}:data:e=1:c=1:w=1"),
        k => panic!("unexpected kind {k} in the {n} row"),
    }
}

/// The capture's member list for `iface`, with `constructor` at the index the capture gives it --
/// the shape the single installer gives a prototype, and the shape every `*_live.rs` gate here pins.
fn expected_proto(iface: &str) -> Vec<String> {
    let fx = bundle();
    let members = fx["interfaces"][iface]["members"]
        .as_array()
        .unwrap_or_else(|| panic!("{iface} absent from the bundle surface"))
        .clone();
    let mut out: Vec<String> = members.iter().map(describe_line).collect();
    // `constructor` AT THE CAPTURE'S SEAT, not appended. It is the last member
    // installed at interface-definition time, so a member a PARTIAL interface adds lands AFTER it --
    // `Navigator` 32 of 66, `Document` 240 of 247, `Screen` 9 of 11 with `onchange` and `isExtended`
    // behind it. Appending it was right on 83 % of the surface and wrong on exactly the interfaces a
    // fingerprinting script reads, and the index has been in the bundle since `3a5f0b60`.
    let at = fx["interfaces"][iface]["ctorIndex"].as_u64().map(|n| n as usize).unwrap_or(out.len()).min(out.len());
    out.insert(at, "constructor:data:e=0:c=1:w=1".to_string());
    out
}

fn str_of(e: &mut Engine, name: &'static str, src: String) -> String {
    e.eval_value(name, src).unwrap().as_str().unwrap().to_string()
}

// -- 1. the nine prototypes, byte-identical to the capture -----------------------------------------

#[tokio::test]
async fn every_l5_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    for iface in IFACES {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = str_of(&mut e, "l5-desc", src);
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(iface), "{iface}.prototype != the capture");
    }
}

#[tokio::test]
async fn every_l5_interface_object_carries_the_capture_tag_length_and_name() {
    let mut e = page().await;
    let fx = bundle();
    for iface in IFACES {
        let src = format!(
            r#"(() => {{
                const C = {iface};
                const t = Object.getOwnPropertyDescriptor(C.prototype, Symbol.toStringTag);
                const native = /\[native code\]/.test(Function.prototype.toString.call(C));
                return [t.value, +t.writable, +t.enumerable, +t.configurable, C.length, C.name, native].join("|");
            }})()"#
        );
        let got = str_of(&mut e, "l5-ctor", src);
        let want_len = fx["interfaces"][iface]["length"].as_u64().unwrap();
        assert_eq!(got, format!("{iface}|0|0|1|{want_len}|{iface}|true"), "{iface} interface object != the capture");
    }
}

// -- 2. the chains, including the three the registry still owns ------------------------------------

/// The prototype chain of each of the nine, against the bundle's `proto` array -- and the same for the
/// three registry interfaces that inherit from two of them.
///
/// THE INSTALL-ORDER GATE. `ViewTimeline`, `CSSAnimation` and `CSSTransition` are still registry
/// shells. The registry's chain pass runs during the ESM layer and cannot resolve a parent the global
/// does not name yet, so it leaves all three on `Object.prototype`; `install::live::rechain_orphans`,
/// which `install_window_abort_style` calls at its end, is the repair. Move `install_window_animations` after L12 and
/// these three lines go back to an empty chain.
#[tokio::test]
async fn every_l5_chain_matches_the_capture_and_the_registry_children_are_rechained() {
    let mut e = page().await;
    let fx = bundle();
    let src = r#"(() => {
        const chain = (n) => {
            const C = globalThis[n];
            if (!C || !C.prototype) return n + ":<absent>";
            const names = [];
            let p = Object.getPrototypeOf(C.prototype);
            while (p && p !== Object.prototype) { names.push(p.constructor ? p.constructor.name : "?"); p = Object.getPrototypeOf(p); }
            return n + ":" + names.join(",");
        };
        return ["Animation", "AnimationEffect", "KeyframeEffect", "AnimationTimeline", "DocumentTimeline",
                "ScrollTimeline", "AnimationEvent", "TransitionEvent", "AnimationPlaybackEvent",
                "ViewTimeline", "CSSAnimation", "CSSTransition"].map(chain).join("\n");
    })()"#;
    let got = str_of(&mut e, "l5-chain", src.to_string());
    let mut want: Vec<String> = IFACES
        .iter()
        .map(|i| {
            let p: Vec<&str> =
                fx["interfaces"][i]["proto"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
            format!("{i}:{}", p.join(","))
        })
        .collect();
    // The three registry children, from the same capture.
    for i in ["ViewTimeline", "CSSAnimation", "CSSTransition"] {
        let p: Vec<&str> =
            fx["interfaces"][i]["proto"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        want.push(format!("{i}:{}", p.join(",")));
    }
    assert_eq!(got.lines().collect::<Vec<_>>(), want, "an L5 prototype chain is not the capture's");
}

/// The constructors: what `new X()` and a bare `X()` answer, recorded from headless Chrome
/// 150.0.7871.46 (the two abstract interfaces throw "Failed to construct 'X': Illegal constructor"
/// for `new` and a bare "Illegal constructor" for a call; the three events and
/// `KeyframeEffect` demand their first argument; the three newable ones mint an object whose
/// `Symbol.toStringTag` is the interface name).
#[tokio::test]
async fn every_l5_constructor_answers_what_chrome_answers() {
    let mut e = page().await;
    let src = r#"(() => {
        const probe = (f) => { try { return String(f()); } catch (err) { return err.constructor.name + ":" + err.message; } };
        const out = [];
        for (const n of ["Animation", "AnimationEffect", "KeyframeEffect", "AnimationTimeline",
                         "DocumentTimeline", "ScrollTimeline", "AnimationEvent", "TransitionEvent",
                         "AnimationPlaybackEvent"]) {
            out.push(n + " new " + probe(() => Object.prototype.toString.call(new globalThis[n]())));
            out.push(n + " call " + probe(() => globalThis[n]()));
        }
        return out.join("\n");
    })()"#;
    let got = str_of(&mut e, "l5-ctors", src.to_string());
    let no_new = |n: &str| {
        format!("{n} call TypeError:Failed to construct '{n}': Please use the 'new' operator, this DOM object constructor cannot be called as a function.")
    };
    let need_arg = |n: &str| format!("{n} new TypeError:Failed to construct '{n}': 1 argument required, but only 0 present.");
    let want = [
        "Animation new [object Animation]".to_string(),
        no_new("Animation"),
        // The `new` shape carries Chrome's construction-context prefix and the bare call
        // does not -- `AnimationEffect` declares no constructor, so there is nothing to construct.
        "AnimationEffect new TypeError:Failed to construct 'AnimationEffect': Illegal constructor".to_string(),
        "AnimationEffect call TypeError:Illegal constructor".to_string(),
        // One argument, not two: `KeyframeEffect` has a second overload, the copy constructor
        // `KeyframeEffect(KeyframeEffect source)` (implemented 2026-10-11, closing this divergence).
        need_arg("KeyframeEffect"),
        no_new("KeyframeEffect"),
        "AnimationTimeline new TypeError:Failed to construct 'AnimationTimeline': Illegal constructor".to_string(),
        "AnimationTimeline call TypeError:Illegal constructor".to_string(),
        "DocumentTimeline new [object DocumentTimeline]".to_string(),
        no_new("DocumentTimeline"),
        "ScrollTimeline new [object ScrollTimeline]".to_string(),
        no_new("ScrollTimeline"),
        need_arg("AnimationEvent"),
        no_new("AnimationEvent"),
        need_arg("TransitionEvent"),
        no_new("TransitionEvent"),
        need_arg("AnimationPlaybackEvent"),
        no_new("AnimationPlaybackEvent"),
    ];
    assert_eq!(got.lines().collect::<Vec<_>>(), want);
}

// -- 3. every minted instance wears the table's prototype ------------------------------------------

/// Failure mode 1 of a table cutover: an instance left on the OLD prototype. Four mints go through
/// `core/animation/mod.rs`'s registered prototypes (`PROTO_ANIMATION`, `PROTO_KEYFRAME_EFFECT`,
/// `PROTO_DOCUMENT_TIMELINE`, `PROTO_PLAYBACK_EVENT`), which `install_window_animations` fills from the
/// table's own install -- if it filled them from a stale prototype, or not at all, every line here
/// goes false or throws.
#[tokio::test]
async fn every_minted_instance_wears_the_tables_prototype() {
    let mut e = page().await;
    let src = r#"(() => {
        const box = document.getElementById("box");
        const a = box.animate({ opacity: [0, 1] }, 1000);
        const out = [];
        out.push("animate:" + (Object.getPrototypeOf(a) === Animation.prototype) + ":" + (a instanceof Animation) +
                 ":" + (a instanceof EventTarget) + ":" + Object.prototype.toString.call(a));
        out.push("effect:" + (Object.getPrototypeOf(a.effect) === KeyframeEffect.prototype) + ":" +
                 (a.effect instanceof KeyframeEffect) + ":" + (a.effect instanceof AnimationEffect) +
                 ":" + Object.prototype.toString.call(a.effect));
        out.push("timeline:" + (Object.getPrototypeOf(a.timeline) === DocumentTimeline.prototype) + ":" +
                 (a.timeline === document.timeline) + ":" + Object.prototype.toString.call(document.timeline));
        out.push("ctor:" + (Object.getPrototypeOf(new Animation()) === Animation.prototype) +
                 ":" + (Object.getPrototypeOf(new KeyframeEffect(box, { opacity: [0, 1] }, 100)) === KeyframeEffect.prototype) +
                 ":" + (Object.getPrototypeOf(new DocumentTimeline()) === DocumentTimeline.prototype) +
                 ":" + (Object.getPrototypeOf(new ScrollTimeline()) === ScrollTimeline.prototype));
        out.push("events:" + (Object.getPrototypeOf(new AnimationEvent("x")) === AnimationEvent.prototype) +
                 ":" + (Object.getPrototypeOf(new TransitionEvent("x")) === TransitionEvent.prototype) +
                 ":" + (Object.getPrototypeOf(new AnimationPlaybackEvent("x")) === AnimationPlaybackEvent.prototype));
        // The `finish` event an animation dispatches is minted from PROTO_PLAYBACK_EVENT, not from
        // globalThis.AnimationPlaybackEvent -- so it survives a page that overwrites the global.
        let seen = "none";
        a.onfinish = (ev) => {
            seen = (Object.getPrototypeOf(ev) === AnimationPlaybackEvent.prototype) + ":" +
                   (ev instanceof AnimationPlaybackEvent) + ":" + ev.type + ":" + (typeof ev.currentTime);
        };
        a.finish();
        out.push("finish-event:" + seen);
        return out.join("\n");
    })()"#;
    let got = str_of(&mut e, "l5-mints", src.to_string());
    assert_eq!(
        got,
        [
            "animate:true:true:true:[object Animation]",
            "effect:true:true:true:[object KeyframeEffect]",
            "timeline:true:true:[object DocumentTimeline]",
            "ctor:true:true:true:true",
            "events:true:true:true",
            "finish-event:true:true:finish:number",
        ]
        .join("\n")
    );
}

/// The registered prototypes are read from the STORE, not from the global, so a page that overwrites
/// `globalThis.Animation` still gets a real `Animation` out of `element.animate()`.
#[tokio::test]
async fn a_page_that_overwrites_the_global_still_gets_a_real_animation() {
    let mut e = page().await;
    let src = r#"(() => {
        const real = Animation.prototype;
        globalThis.Animation = function Fake() {};
        const a = document.getElementById("box").animate({ opacity: [0, 1] }, 1000);
        const ok = Object.getPrototypeOf(a) === real && typeof a.play === "function" && a.playState === "running";
        return String(ok) + ":" + Object.prototype.toString.call(a);
    })()"#;
    let got = str_of(&mut e, "l5-overwrite", src.to_string());
    assert_eq!(got, "true:[object Animation]");
}

// -- 4. no row accepts a foreign receiver ----------------------------------------------------------

/// Failure mode 2: a row that answers for a receiver that is not its interface.
///
/// A REFUSAL HAS TWO SHAPES, AND THIS TEST KNEW ONLY ONE. Re-measured on headless Chrome
/// 150.0.7871.46 -- the same build the original line was recorded from -- over a loopback server
/// with `--dump-dom`:
///
/// ```text
///   get Animation.prototype.ready .call({})     REJECTED TypeError: Failed to read the 'ready'
///                                                 property from 'Animation': Illegal invocation
///   get Animation.prototype.finished .call({})  REJECTED TypeError: Failed to read the 'finished'
///                                                 property from 'Animation': Illegal invocation
///   Animation.prototype.play.call({})           SYNC-THROW TypeError: Illegal invocation
/// ```
///
/// A `Promise`-typed member refuses by REJECTING: WebIDL turns the receiver `TypeError` raised
/// while reading a promise-typed attribute into a rejected promise, so it never throws
/// synchronously. `install/receiver.rs` rule 2 is that rule, read off `MemberDef::ty`. Demanding a
/// synchronous throw from EVERY row pinned korvus's divergence on `ready` / `finished` instead of
/// Chrome's answer.
///
/// A RESOLVED promise, or any non-promise return, is still a failure, so this is the same strength
/// of check in the right shape.
#[tokio::test]
async fn the_animation_and_effect_rows_refuse_a_foreign_receiver() {
    let mut e = page().await;
    // Phase 1: probe every row. A promise-shaped refusal is recorded when it SETTLES, so the
    // verdict is read after the microtask checkpoint below rather than guessed at here.
    let src = r#"(() => {
        const bad = [];
        globalThis.__animBad = bad;
        for (const iface of ["Animation", "AnimationEffect", "KeyframeEffect"]) {
            const proto = globalThis[iface].prototype;
            for (const name of Object.getOwnPropertyNames(proto)) {
                if (name === "constructor") continue;
                const d = Object.getOwnPropertyDescriptor(proto, name);
                // The three `on*` handler rows are EventTarget's accessor pair; a foreign receiver
                // reads null there in Chrome too, so they are not part of this check.
                if (name.startsWith("on")) continue;
                const label = iface + "." + name;
                try {
                    const r = d.get ? d.get.call({}) : d.value.call({});
                    if (r && typeof r.then === "function") {
                        r.then(
                            () => bad.push(label + " = RESOLVED (a refusal must reject)"),
                            err => {
                                if (!(err instanceof TypeError) || String(err.message).indexOf("Illegal invocation") < 0) {
                                    bad.push(label + " = REJECTED:" + err.name + ":" + err.message);
                                }
                            });
                    } else {
                        bad.push(label + " = no-throw:" + Object.prototype.toString.call(r));
                    }
                } catch (err) {
                    if (err.constructor.name !== "TypeError") bad.push(label + " = " + err.constructor.name);
                }
            }
        }
        return "probed";
    })()"#;
    assert_eq!(str_of(&mut e, "l5-foreign", src.to_string()), "probed");
    // Phase 2: run the microtask checkpoint, so every rejection above has been observed.
    e.pump(std::time::Duration::from_millis(50)).await.expect("pump");
    let got = str_of(&mut e, "l5-foreign-read", r#"(globalThis.__animBad.join("\n") || "all-refuse")"#.to_string());
    assert_eq!(got, "all-refuse", "a row answered a foreign receiver where Chrome throws or rejects");
}

/// The four timeline rows, which had NO receiver check before this lane and answered null / "block"
/// off any object at all. Every line is recorded from headless Chrome 150.0.7871.46; see the
/// brand-check block in `core/animation/document_timeline.rs` for the full transcript and for the one
/// residual (`Object.create(proto)` passes here and throws in Chrome).
#[tokio::test]
async fn the_timeline_rows_refuse_a_foreign_receiver() {
    let mut e = page().await;
    let src = r#"(() => {
        const g = (i, n) => Object.getOwnPropertyDescriptor(globalThis[i].prototype, n).get;
        const probe = (label, f) => {
            let r;
            try { r = String(f()); } catch (err) { r = err.constructor.name + ":" + err.message; }
            return label + " = " + r;
        };
        const out = [];
        for (const n of ["currentTime", "duration"]) {
            out.push(probe("AnimationTimeline." + n + " on {}", () => g("AnimationTimeline", n).call({})));
            out.push(probe("AnimationTimeline." + n + " on window", () => g("AnimationTimeline", n).call(window)));
        }
        for (const n of ["source", "axis"]) {
            out.push(probe("ScrollTimeline." + n + " on {}", () => g("ScrollTimeline", n).call({})));
            out.push(probe("ScrollTimeline." + n + " on document.timeline", () => g("ScrollTimeline", n).call(document.timeline)));
        }
        // ...and every receiver that IS one still answers, subclasses included.
        out.push(probe("AnimationTimeline.currentTime on document.timeline", () => typeof g("AnimationTimeline", "currentTime").call(document.timeline)));
        out.push(probe("AnimationTimeline.currentTime on new ScrollTimeline", () => g("AnimationTimeline", "currentTime").call(new ScrollTimeline())));
        out.push(probe("AnimationTimeline.duration on new ScrollTimeline", () => g("AnimationTimeline", "duration").call(new ScrollTimeline())));
        out.push(probe("ScrollTimeline.axis on new ScrollTimeline", () => g("ScrollTimeline", "axis").call(new ScrollTimeline())));
        out.push(probe("ScrollTimeline.source on new ViewTimeline", () => g("ScrollTimeline", "source").call(new ViewTimeline())));
        out.push(probe("ScrollTimeline.axis on new ViewTimeline", () => g("ScrollTimeline", "axis").call(new ViewTimeline())));
        out.push(probe("AnimationTimeline.currentTime on new ViewTimeline", () => g("AnimationTimeline", "currentTime").call(new ViewTimeline())));
        return out.join("\n");
    })()"#;
    let got = str_of(&mut e, "l5-timeline-brand", src.to_string());
    let illegal = "TypeError:Illegal invocation";
    assert_eq!(
        got,
        [
            format!("AnimationTimeline.currentTime on {{}} = {illegal}"),
            format!("AnimationTimeline.currentTime on window = {illegal}"),
            format!("AnimationTimeline.duration on {{}} = {illegal}"),
            format!("AnimationTimeline.duration on window = {illegal}"),
            format!("ScrollTimeline.source on {{}} = {illegal}"),
            format!("ScrollTimeline.source on document.timeline = {illegal}"),
            format!("ScrollTimeline.axis on {{}} = {illegal}"),
            format!("ScrollTimeline.axis on document.timeline = {illegal}"),
            "AnimationTimeline.currentTime on document.timeline = number".to_string(),
            "AnimationTimeline.currentTime on new ScrollTimeline = null".to_string(),
            "AnimationTimeline.duration on new ScrollTimeline = null".to_string(),
            "ScrollTimeline.axis on new ScrollTimeline = block".to_string(),
            "ScrollTimeline.source on new ViewTimeline = null".to_string(),
            "ScrollTimeline.axis on new ViewTimeline = block".to_string(),
            "AnimationTimeline.currentTime on new ViewTimeline = null".to_string(),
        ]
        .join("\n")
    );
}

/// THE DAY IT CHANGED, and it was chosen: a change gave THE INSTALLER a receiver check derived
/// from the table, so every row with a body -- the shared `slot_get` kit included -- refuses a
/// receiver that is not an instance.
///
/// This test used to pin the opposite, because the answer came from `core/events/event.rs`'s
/// `cb_evt_slot_get`, which reads a private slot off `this`, finds none, and says `undefined` where
/// every browser throws. Its own note said "the day it changes is a day somebody chose, and it is a
/// change to one shared body, not nine rows here" -- which is what happened, one level further out
/// still: the check is not in the slot getter but in `install::receiver`, applied by
/// `installer::put_member` to every `Install` and `slot` row of every table, so no body has to
/// remember.
///
/// RE-ASKED CHROME 152.0.7977.82 for every line below (headless, no DevTools) rather than writing down
/// what the engine now says. All seven throw `TypeError: Illegal invocation` -- `AnimationEvent.animation`
/// too, which Chrome 152 exposes (Chrome 150 did not: its descriptor was `undefined`, and reading `.get` off it
/// threw before any receiver was checked).
#[tokio::test]
async fn the_event_slot_rows_refuse_a_foreign_receiver() {
    let mut e = page().await;
    let src = r#"(() => {
        const g = (i, n) => Object.getOwnPropertyDescriptor(globalThis[i].prototype, n).get;
        const probe = (i, n) => {
            let r;
            try { r = String(g(i, n).call({})); } catch (err) { r = err.constructor.name + ":" + err.message; }
            return i + "." + n + " = " + r;
        };
        return [
            probe("AnimationEvent", "animationName"),
            probe("AnimationEvent", "elapsedTime"),
            probe("AnimationEvent", "animation"),
            probe("TransitionEvent", "propertyName"),
            probe("TransitionEvent", "pseudoTarget"),
            probe("AnimationPlaybackEvent", "currentTime"),
            probe("AnimationPlaybackEvent", "timelineTime"),
        ].join("\n");
    })()"#;
    let got = str_of(&mut e, "l5-event-brand", src.to_string());
    assert_eq!(
        got,
        [
            "AnimationEvent.animationName = TypeError:Illegal invocation",
            "AnimationEvent.elapsedTime = TypeError:Illegal invocation",
            "AnimationEvent.animation = TypeError:Illegal invocation",
            "TransitionEvent.propertyName = TypeError:Illegal invocation",
            "TransitionEvent.pseudoTarget = TypeError:Illegal invocation",
            "AnimationPlaybackEvent.currentTime = TypeError:Illegal invocation",
            "AnimationPlaybackEvent.timelineTime = TypeError:Illegal invocation",
        ]
        .join("\n"),
        "a slot-backed row stopped refusing a foreign receiver -- Chrome 152.0.7977.82 throws \
         `Illegal invocation` for every one of these, and the check is the installer's \
         (`install::receiver`), not this lane's"
    );
}

// -- 5. the arity guards ---------------------------------------------------------------------------

/// The two rows that declare a required argument. The nine left `generated_order.rs` with the tables,
/// so `op_finalize_surface`'s arity pass does not reach them any more: this is the row's own guard.
#[tokio::test]
async fn the_two_required_argument_rows_carry_their_arity_guard() {
    let mut e = page().await;
    let src = r#"(() => {
        const box = document.getElementById("box");
        const probe = (f) => { try { f(); return "no-throw"; } catch (err) { return err.constructor.name + ":" + err.message; } };
        const a = new Animation();
        const k = new KeyframeEffect(box, { opacity: [0, 1] }, 100);
        return [
            probe(() => a.updatePlaybackRate()),
            probe(() => k.setKeyframes()),
            String(Animation.prototype.updatePlaybackRate.length) + ":" + String(KeyframeEffect.prototype.setKeyframes.length),
            String(/\[native code\]/.test(Function.prototype.toString.call(Animation.prototype.updatePlaybackRate))),
        ].join("\n");
    })()"#;
    let got = str_of(&mut e, "l5-arity", src.to_string());
    assert_eq!(
        got,
        [
            "TypeError:Failed to execute 'updatePlaybackRate' on 'Animation': 1 argument required, but only 0 present.",
            "TypeError:Failed to execute 'setKeyframes' on 'KeyframeEffect': 1 argument required, but only 0 present.",
            "1:1",
            "true",
        ]
        .join("\n")
    );
}

// -- 6. the values the bundle's oracle records -----------------------------------------------------

/// The answers the bundle's own oracle files record for these interfaces, read back out of the
/// tables. `behavior_runner_async` grades the full roster; this is the subset that would go silent if
/// a row lost its body in the move, plus the two timeline kinds telling themselves apart.
#[tokio::test]
async fn the_recorded_answers_survive_the_move() {
    let mut e = page().await;
    let src = r#"(() => {
        const box = document.getElementById("box");
        const out = [];
        out.push("doc-timeline:" + Object.prototype.toString.call(document.timeline.currentTime) +
                 ":" + Object.prototype.toString.call(document.timeline.duration));
        const st = new ScrollTimeline();
        out.push("scroll:" + st.axis + ":" + Object.prototype.toString.call(st.source) +
                 ":" + st.currentTime + ":" + st.duration);
        const vt = globalThis.ViewTimeline ? new ViewTimeline() : null;
        out.push("view:" + (vt ? vt.axis + ":" + vt.source + ":" + vt.currentTime : "<absent>"));
        const a = box.animate({ opacity: [0, 1] }, 1000);
        out.push("anim:" + a.playState + ":" + a.pending + ":" + a.replaceState + ":" + (typeof a.startTime) +
                 ":" + (a.finished instanceof Promise) + ":" + (a.ready instanceof Promise) + ":" + JSON.stringify(a.id));
        const t = a.effect.getTiming();
        const c = a.effect.getComputedTiming();
        out.push("timing:" + t.duration + ":" + t.fill + ":" + t.easing + ":" + c.endTime + ":" + (typeof c.progress));
        out.push("effect:" + (a.effect.target === box) + ":" + a.effect.pseudoElement + ":" + a.effect.composite +
                 ":" + a.effect.getKeyframes().length);
        const ae = new AnimationEvent("x");
        out.push("animevent:" + JSON.stringify([ae.animationName, ae.elapsedTime, ae.pseudoElement, ae.animation, ae.pseudoTarget]));
        const te = new TransitionEvent("y", { propertyName: "opacity", elapsedTime: 1.5 });
        out.push("transevent:" + JSON.stringify([te.propertyName, te.elapsedTime, te.pseudoElement, te.animation, te.pseudoTarget]));
        const pe = new AnimationPlaybackEvent("z");
        out.push("playback:" + JSON.stringify([pe.currentTime, pe.timelineTime, pe.type]));
        return out.join("\n");
    })()"#;
    let got = str_of(&mut e, "l5-values", src.to_string());
    assert_eq!(
        got,
        [
            "doc-timeline:[object Number]:[object Null]",
            "scroll:block:[object HTMLHtmlElement]:null:null",
            "view:block:null:null",
            "anim:running:false:active:number:true:true:\"\"",
            "timing:1000:auto:linear:1000:number",
            "effect:true:null:replace:2",
            r#"animevent:["",0,"",null,null]"#,
            r#"transevent:["opacity",1.5,"",null,null]"#,
            r#"playback:[null,null,"z"]"#,
        ]
        .join("\n")
    );
}

/// A script that writes an animation between two style reads of ONE task reads what it wrote: the reads share a
/// moment, and a style flush within a moment reuses the last tick's values only while no animation was written
/// since (`animation::css::tick`'s memo, keyed on the store's write count). Seeking, retiming, pausing and
/// cancelling each move the value the next read reports, whatever reads came between.
#[tokio::test]
async fn an_animation_written_between_two_reads_of_one_task_reads_its_new_value() {
    let mut e = page().await;
    let src = r#"(() => {
        const box = document.getElementById("box");
        const read = () => { for (let i = 0; i < 50; i++) box.offsetHeight; return getComputedStyle(box).opacity; };
        const a = box.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 1000 });
        a.pause();
        a.currentTime = 250;
        const out = [read(), read()];
        a.currentTime = 750;
        out.push(read());
        a.effect.updateTiming({ duration: 3000 });
        out.push(read());
        a.effect.setKeyframes([{ opacity: 1 }, { opacity: 0 }]);
        out.push(read());
        a.cancel();
        out.push(read());
        return out.join(",");
    })()"#;
    assert_eq!(str_of(&mut e, "memo-writes", src.to_string()), "0.25,0.25,0.75,0.25,0.75,1");
}
