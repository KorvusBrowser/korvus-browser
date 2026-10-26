//! `MediaQueryList` and `window.matchMedia`, read back off the LIVE surface.
//!
//! `host.js`'s `__wireMQL` REDEFINED five members of this prototype at `INSTALL_DOCUMENT` time, with
//! `op_nativize`-masked JS bodies, over rows the registry install had already mounted — the largest
//! single group in `tests/fixtures/second_writers.manifest`. This file is the gate on the table that
//! took their place.
//!
//! EVERY EXPECTATION MARKED "measured" comes from `google-chrome --headless=new` 150.0.7871.46 over a
//! loopback HTTP server, 2026-09-12.

use engine::Engine;

async fn probe(script: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body>x</body></html>").unwrap();
    e.eval("t", script.to_string()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

/// The prototype's shape: the capture's five members in the capture's order, `constructor` last (as
/// Chrome has it), the `EventTarget` chain, and the non-constructible interface object.
#[tokio::test]
async fn the_prototype_has_the_recorded_shape() {
    let out = probe(
        r#"
        print("keys=" + JSON.stringify(Object.getOwnPropertyNames(MediaQueryList.prototype)));
        print("chain=" + (Object.getPrototypeOf(MediaQueryList.prototype) === EventTarget.prototype));
        print("tag=" + Object.prototype.toString.call(matchMedia("(min-width: 0px)")));
        print("own=" + JSON.stringify(Object.getOwnPropertyNames(matchMedia("(min-width: 0px)"))));
        print("instance=" + (matchMedia("(min-width: 0px)") instanceof MediaQueryList));
        print("newable=" + (() => { try { new MediaQueryList(); return "constructed"; } catch (e) { return e.constructor.name; } })());
        const P = MediaQueryList.prototype;
        print("fns=" + [P.addListener, P.removeListener].map((f) => f.name + "/" + f.length + "/" + String(f).includes("[native code]")).join(","));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            r#"keys=["media","matches","onchange","addListener","removeListener","constructor"]"#,
            "chain=true",
            "tag=[object MediaQueryList]",
            "own=[]",
            "instance=true",
            "newable=TypeError",
            "fns=addListener/1/true,removeListener/1/true",
        ]
    );
}

/// `media` SERIALIZES the query and `matches` runs the cascade's own evaluator.
///
/// `(MIN-WIDTH: 0PX)` reading `(min-width: 0px)` and `matchMedia("(min-width: 0px)").matches` being
/// `true` are both measured in Chrome. An unparseable list serializing to `not all` is CSSOM's rule
/// and `cssom::media_serialize`'s, unchanged by this cutover.
#[tokio::test]
async fn media_serializes_and_matches_runs_the_cascades_evaluator() {
    let out = probe(
        r#"
        print("normalized=" + matchMedia("(MIN-WIDTH: 0PX)").media);
        print("invalid=" + matchMedia("garbage (").media);
        print("matches=" + matchMedia("(min-width: 0px)").matches);
        print("never=" + matchMedia("(min-width: 999999px)").matches);
        // `matchMedia` is an own property of the global, as Chrome reports it (measured:
        // `own truetruetrue`), and it is the `Window` table's row.
        const d = Object.getOwnPropertyDescriptor(globalThis, "matchMedia");
        print("row=" + d.writable + d.enumerable + d.configurable + " " + matchMedia.name + "/" + matchMedia.length
            + " " + String(matchMedia).includes("[native code]"));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "normalized=(min-width: 0px)",
            "invalid=not all",
            "matches=true",
            "never=false",
            "row=truetruetrue matchMedia/1 true",
        ]
    );
}

/// The two Level-3 aliases really are `add`/`removeEventListener("change")`, and `onchange` is the
/// realm's ONE handler store — so a handler set through either route runs exactly once per event.
#[tokio::test]
async fn the_legacy_aliases_forward_to_the_event_target() {
    let out = probe(
        r#"
        const mql = matchMedia("(min-width: 0px)");
        let viaAlias = 0, viaHandler = 0;
        const h = () => { viaAlias++; };
        mql.addListener(h);
        mql.onchange = () => { viaHandler++; };
        mql.dispatchEvent(new Event("change"));
        print("after1=" + viaAlias + "," + viaHandler);
        mql.removeListener(h);
        mql.dispatchEvent(new Event("change"));
        print("after2=" + viaAlias + "," + viaHandler);
        // The handler round-trips through the store, which a plain data property would also do —
        // what it would NOT do is fire on dispatch, which `after1` proves it does.
        print("handlerReadBack=" + (typeof mql.onchange === "function"));
        print("unsetIsNull=" + String(matchMedia("(min-width: 0px)").onchange));
        "#,
    )
    .await;
    assert_eq!(out, vec!["after1=1,1", "after2=1,2", "handlerReadBack=true", "unsetIsNull=null"]);
}

/// THE RECEIVER CHECK AND THE ARITY GUARD, both of which redefining the members had stripped off.
///
/// All five rows throw `Illegal invocation` for a foreign receiver in Chrome (measured, `onchange`'s
/// getter included). `matchMedia.call(null, q)` answers a `MediaQueryList` in Chrome -- V8 hands a
/// null receiver the global; a foreign `{}` is `Illegal invocation`
/// (`frame_globals_live.rs`) -- and both of its TypeErrors carry Chrome's wording.
#[tokio::test]
async fn no_row_accepts_a_foreign_receiver_and_the_arity_guards_are_back() {
    let out = probe(
        r#"
        const P = MediaQueryList.prototype;
        const err = (label, f) => { try { print(label + "=NO THROW:" + String(f())); } catch (e) { print(label + "=" + e.constructor.name + ": " + e.message); } };
        err("media", () => Object.getOwnPropertyDescriptor(P, "media").get.call({}));
        err("matches", () => Object.getOwnPropertyDescriptor(P, "matches").get.call({}));
        err("onchange", () => Object.getOwnPropertyDescriptor(P, "onchange").get.call({}));
        err("addListener", () => P.addListener.call({}, () => {}));
        err("removeListener", () => P.removeListener.call({}, () => {}));
        err("addListenerNoArgs", () => matchMedia("(min-width: 0px)").addListener());
        err("matchMediaNoArgs", () => matchMedia());
        err("matchMediaSymbol", () => matchMedia(Symbol("a")));
        // No receiver check on `matchMedia`, measured.
        print("matchMediaForeign=" + Object.prototype.toString.call(matchMedia.call(null, "(min-width: 0px)")));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "media=TypeError: Illegal invocation",
            "matches=TypeError: Illegal invocation",
            "onchange=TypeError: Illegal invocation",
            "addListener=TypeError: Illegal invocation",
            "removeListener=TypeError: Illegal invocation",
            "addListenerNoArgs=TypeError: Failed to execute 'addListener' on 'MediaQueryList': 1 argument required, but only 0 present.",
            "matchMediaNoArgs=TypeError: Failed to execute 'matchMedia' on 'Window': 1 argument required, but only 0 present.",
            "matchMediaSymbol=TypeError: Failed to execute 'matchMedia' on 'Window': Cannot convert a Symbol value to a string",
            "matchMediaForeign=[object MediaQueryList]",
        ]
    );
}
