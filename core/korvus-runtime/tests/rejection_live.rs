//! Unhandled promise rejections, reported natively.
//!
//! HTML's "notify about rejected promises" was the last piece of error reporting still written in
//! JavaScript: a closure inside the kernel's `BOOTSTRAP` string handed to
//! `Deno.core.setUnhandledPromiseRejectionHandler`, pushing into the page-visible array
//! `KB.__korvusRejections`. Three things had to hold and one of them did not:
//!
//!   * a rejection must NOT abort the event loop (deno_core's default makes it fatal) — held;
//!   * `unhandledrejection` must be dispatched at the global — held;
//!   * the event must CARRY the rejection. `PromiseRejectionEvent` was a registry interface with two
//!     `stub` rows, so `event.promise` and `event.reason` were both `undefined` — the entire point of
//!     the event — on every rejection this engine had ever fired.

use engine::Engine;

async fn probe(script: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("t", script.to_string()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

/// The event a REJECTION delivers, against the shape measured in `google-chrome --headless=new` 141
/// on 2026-09-12:
///
/// ```text
/// brand [object PromiseRejectionEvent]   reason  the rejection reason   promise  a Promise
/// cancelable true   bubbles false   isTrusted true   own-property count 1   target  window
/// ```
#[tokio::test]
async fn an_unhandled_rejection_fires_an_event_that_carries_it() {
    let out = probe(
        r#"
        globalThis.addEventListener("unhandledrejection", (ev) => {
          print("brand=" + Object.prototype.toString.call(ev));
          print("reason=" + (ev.reason && ev.reason.message));
          print("promise=" + (ev.promise instanceof Promise));
          print("flags=" + ev.cancelable + " " + ev.bubbles + " " + ev.isTrusted);
          print("own=" + Object.getOwnPropertyNames(ev).length);
          print("target=" + (ev.target === globalThis));
        });
        Promise.reject(new TypeError("boom"));
        print("alive=yes");
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "alive=yes",
            "brand=[object PromiseRejectionEvent]",
            "reason=boom",
            "promise=true",
            "flags=true false true",
            "own=1",
            "target=true",
        ]
    );
}

/// `onunhandledrejection` is a real event-handler IDL attribute on the Window table, so the dispatch
/// runs it — ONCE. The deleted JS also invoked it by hand when `dispatchEvent` was missing, which
/// delivered the same rejection twice the moment the handler became a real accessor.
#[tokio::test]
async fn the_handler_attribute_runs_exactly_once() {
    let out = probe(
        r#"
        let n = 0;
        globalThis.onunhandledrejection = (ev) => { n++; print("handler reason=" + ev.reason); };
        Promise.reject("once");
        // A 0 ms timer set by the script is a task queued BEFORE the rejection's notification (queued at the
        // script's microtask checkpoint), so it reads 0 -- desktop Chrome 150, 3/3 alike -- and a
        // later one reads the one call.
        setTimeout(() => print("count=" + n), 0);
        setTimeout(() => print("later=" + n), 50);
        "#,
    )
    .await;
    assert_eq!(out, vec!["count=0", "handler reason=once", "later=1"]);
}

/// A page that keeps rejecting KEEPS RUNNING, which is what returning `true` from the handler buys:
/// deno_core's default surfaces an unhandled rejection as a fatal event-loop error.
#[tokio::test]
async fn a_rejecting_page_survives_and_the_engine_records_every_one() {
    let out = probe(
        r#"
        for (let i = 0; i < 3; i++) Promise.reject(new Error("r" + i));
        setTimeout(() => print("timers still run"), 0);
        print("sync ok");
        "#,
    )
    .await;
    assert_eq!(out, vec!["sync ok", "timers still run"]);
}

/// The CONSTRUCTOR, which is the same one the native path mints through — so a page cannot tell an
/// engine-delivered event from one it built. Chrome's own messages, measured 2026-09-12.
#[tokio::test]
async fn the_constructor_is_chromes() {
    let out = probe(
        r#"
        const why = (f) => { try { f(); return "no throw"; } catch (e) { return e.constructor.name + ": " + e.message; } };
        print("noArgs=" + why(() => new PromiseRejectionEvent()));
        print("oneArg=" + why(() => new PromiseRejectionEvent("x")));
        print("emptyInit=" + why(() => new PromiseRejectionEvent("x", {})));
        const p = Promise.resolve();
        const e = new PromiseRejectionEvent("unhandledrejection", { promise: p, reason: 1, cancelable: true });
        print("built=" + e.type + " " + (e.promise === p) + " " + e.reason + " " + e.cancelable + " " + Object.getOwnPropertyNames(e).length);
        print("noReason=" + typeof new PromiseRejectionEvent("y", { promise: p }).reason);
        print("shape=" + PromiseRejectionEvent.length + " " + (Object.getPrototypeOf(PromiseRejectionEvent.prototype) === Event.prototype));
        print("native=" + Function.prototype.toString.call(Object.getOwnPropertyDescriptor(PromiseRejectionEvent.prototype, "reason").get).includes("[native code]"));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "noArgs=TypeError: Failed to construct 'PromiseRejectionEvent': 2 arguments required, but only 0 present.",
            "oneArg=TypeError: Failed to construct 'PromiseRejectionEvent': 2 arguments required, but only 1 present.",
            "emptyInit=TypeError: Failed to construct 'PromiseRejectionEvent': Failed to read the 'promise' property from 'PromiseRejectionEventInit': Required member is undefined.",
            "built=unhandledrejection true 1 true 1",
            "noReason=undefined",
            "shape=2 true",
            "native=true",
        ]
    );
}
