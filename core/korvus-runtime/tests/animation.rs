//! The Web Animations timeline, end to end in a booted engine (animation-timeline, Track 3.2).
//!
//! `core/korvus-page`'s unit tests already prove the ARITHMETIC (play states, easing curves, the
//! timing pipeline) without V8. This file proves the two things only a running engine can show:
//!
//!   1. `element.animate()` returns a real `Animation` that is RUNNING, whose `currentTime` ADVANCES
//!      between two pumps of the event loop, and which appears in both `element.getAnimations()` and
//!      `document.getAnimations()`;
//!   2. `pause()` freezes `currentTime`, `finish()` reaches "finished" and resolves the `finished`
//!      promise, and `cancel()` takes the animation back out of `getAnimations()`.
//!
//! The advance is measured across `e.run().await`, not across a busy loop: "the clock moves while JS
//! is not running" is the property that matters, and it is the one a stubbed timeline cannot fake.
use engine::Engine;
const PAGE: &str = "<!doctype html><html><head></head><body><div id=box></div></body></html>";

async fn boot() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();
    e
}

/// Eval and return the printed lines.
async fn lines(e: &mut Engine, src: &str) -> Vec<String> {
    e.eval("anim", src.to_string()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

#[tokio::test]
async fn animate_creates_a_running_animation_whose_current_time_advances() {
    let mut e = boot().await;

    let out = lines(
        &mut e,
        r#"
        const box = document.getElementById("box");
        globalThis.__a = box.animate({ opacity: [0, 1] }, 1000);
        print("ctor=" + Object.prototype.toString.call(globalThis.__a));
        print("state=" + globalThis.__a.playState);
        print("effect=" + Object.prototype.toString.call(globalThis.__a.effect));
        print("target=" + (globalThis.__a.effect.target === box));
        print("timeline=" + (globalThis.__a.timeline === document.timeline));
        print("inElement=" + box.getAnimations().includes(globalThis.__a));
        print("inDocument=" + document.getAnimations().includes(globalThis.__a));
        globalThis.__t0 = globalThis.__a.currentTime;
        print("t0type=" + typeof globalThis.__t0);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "ctor=[object Animation]",
            "state=running",
            "effect=[object KeyframeEffect]",
            "target=true",
            "timeline=true",
            "inElement=true",
            "inDocument=true",
            "t0type=number",
        ]
    );

    // ── the load-bearing assertion: the timeline moves between two pumps of the event loop. ──
    // A `setTimeout` gives the loop real work to wait on, so the gap is wall-clock time passing
    // outside JS rather than a spin.
    e.eval("tick", "setTimeout(() => { globalThis.__t1 = globalThis.__a.currentTime; }, 30);".into())
        .unwrap();
    for _ in 0..200 {
        e.run().await.unwrap();
        let done = e
            .eval_value("check", "globalThis.__t1 !== undefined".into())
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if done {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let out = lines(
        &mut e,
        r#"
        print("advanced=" + (globalThis.__t1 > globalThis.__t0));
        print("plausible=" + (globalThis.__t1 - globalThis.__t0 >= 10 && globalThis.__t1 - globalThis.__t0 < 5000));
        print("coherent=" + (Math.abs(document.timeline.currentTime - performance.now()) < 50));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec!["advanced=true", "plausible=true", "coherent=true"],
        "currentTime must advance with the event loop, on the same clock performance.now() reads"
    );
}

#[tokio::test]
async fn pause_finish_and_cancel_move_the_animation_through_its_states() {
    let mut e = boot().await;
    let out = lines(
        &mut e,
        r#"
        const box = document.getElementById("box");
        const a = box.animate({ opacity: [0, 1] }, 100000);
        a.pause();
        const p0 = a.currentTime;
        // Burn some real time; a paused animation must not move.
        const start = performance.now();
        while (performance.now() - start < 5) {}
        print("paused=" + a.playState + " frozen=" + (a.currentTime === p0));

        let resolved = false;
        a.finished.then(() => { resolved = true; });
        a.finish();
        print("finished=" + a.playState + " at=" + (a.currentTime === 100000));
        globalThis.__check = () => resolved;

        a.cancel();
        print("cancelled=" + a.playState);
        print("goneFromElement=" + (box.getAnimations().length === 0));
        print("goneFromDocument=" + (document.getAnimations().length === 0));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "paused=paused frozen=true",
            "finished=finished at=true",
            "cancelled=idle",
            "goneFromElement=true",
            "goneFromDocument=true",
        ]
    );
    // The `finished` promise settles on the microtask checkpoint, which the pump above ran.
    let resolved = e.eval_value("resolved", "globalThis.__check()".into()).unwrap();
    assert_eq!(resolved.as_bool(), Some(true), "finish() must resolve the `finished` promise");
}

#[tokio::test]
async fn a_bare_animation_matches_the_recorded_browser() {
    // The answers Brave 151 gives for `new Animation()` — the shape every one of this unit's
    // behavior cases is written against, checked here too so a regression shows up in a fast test
    // rather than only in the 200-second surface harness.
    let mut e = boot().await;
    let out = lines(
        &mut e,
        r#"
        const a = new Animation();
        print([a.startTime, a.currentTime, a.effect, a.overallProgress].map(String).join(","));
        print(a.id + "|" + a.playbackRate + "|" + a.playState + "|" + a.replaceState + "|" + a.pending);
        print(Object.prototype.toString.call(a.timeline) + Object.prototype.toString.call(a.ready)
              + Object.prototype.toString.call(a.finished));
        print("own=" + Object.keys(a).length);
        let threw = ""; try { a.commitStyles(); } catch (err) { threw = err.name; }
        print("commitStyles=" + threw);
        let arity = ""; try { a.updatePlaybackRate(); } catch (err) { arity = err.name; }
        print("updatePlaybackRate=" + arity);
        print("native=" + /\[native code\]/.test(Animation.toString())
              + /\[native code\]/.test(Object.getOwnPropertyDescriptor(Animation.prototype, "playState").get.toString()));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "null,null,null,null",
            "|1|idle|active|false",
            "[object DocumentTimeline][object Promise][object Promise]",
            // A real DOM object owns no data property; everything is a prototype accessor over a
            // private slot. `Object.keys(new Animation()).length` is a one-line probe.
            "own=0",
            "commitStyles=NoModificationAllowedError",
            "updatePlaybackRate=TypeError",
            "native=truetrue",
        ]
    );
}
