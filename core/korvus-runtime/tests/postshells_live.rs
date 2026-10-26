//! LIVE gate for the two passes that must be LAST.
//!
//! `js/dom/09-postshells.js` existed for one structural reason: it was the extension's ESM entry
//! point, so its body ran after every other install, and it ended by calling the three
//! surface-finishing passes -- the WebIDL arity guards (which wrap whatever owns each member slot at
//! the end of the bootstrap), the enumeration-order restore (which has to see the finished
//! prototype), and the constant seal. Lane L18 moved those calls into Rust:
//! `install::live::install_window_postshells`, the first line of the runtime's pre-bootstrap block, which is
//! the statement immediately after the `JsRuntime::new` that evaluates that module -- the identical
//! position in the order -- and `Engine::install_document` for the second run.
//!
//! A cutover of a positional mechanism is only as good as the property the position bought, so this
//! file reads that property back off the LIVE surface at the end of bootstrap, rather than trusting
//! that a call moved cleanly:
//!
//!   1. **The first run's guards hold.** A registry method with WebIDL-required arguments throws the
//!      arity TypeError, and one with none does not.
//!   2. **The SECOND run's guards hold** -- the half the old mechanism silently lost. `BOOTSTRAP`'s
//!      CSSOM block assigns `CSSStyleSheet.prototype.replace`/`replaceSync` and `host.js`'s
//!      `__wireMQL` assigned `MediaQueryList`'s legacy listener aliases, both AFTER the first run, so
//!      the pass has to run once more behind them. (`__wireMQL` is deleted: `MediaQueryList`
//!      is a table installed once, and its two aliases are guarded by their own `required 1` rows --
//!      which is why the case below still passes with the second writer gone.) It did not run, for
//!      weeks: the JS re-run read a
//!      `Deno.core.ops` the bootstrap had already deleted and threw `ReferenceError` into a silent
//!      `catch` (found in the status ledger, not in a test -- this is that test).
//!   3. **The recorded enumeration order holds.** `Object.getOwnPropertyNames(X.prototype)` is three
//!      lines for a page to read and no two engines produce the same sequence by accident, so the
//!      order pass is a stealth axis, not a tidiness one. Asserted against the BUNDLE's recorded
//!      order, never against the engine's own opinion.
//!   4. **The constant seal holds**: a WebIDL constant the capture records as non-configurable is
//!      non-configurable here.

use engine::Engine;

const PAGE: &str = r#"<!doctype html><html><head><style>div { color: red }</style></head>
<body><div id="d">x</div></body></html>"#;

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

fn str_of(e: &mut Engine, name: &'static str, src: String) -> String {
    e.eval_value(name, src).unwrap().as_str().unwrap().to_string()
}

/// `expr` as "ok:<value>" or "throw:<Ctor>:<message>" -- a thrown error and a returned value are both
/// reportable, so a member that stops throwing reads as a diff instead of a panic.
fn probe(e: &mut Engine, expr: &str) -> String {
    let src = format!(
        r#"(() => {{ try {{ const v = ({expr}); return "ok:" + String(v); }}
                     catch (err) {{ return "throw:" + err.constructor.name + ":" + err.message; }} }})()"#
    );
    str_of(e, "l18-probe", src)
}

// -- 1. the first run: the registry's own arity guards -------------------------------------------

#[tokio::test]
async fn the_first_finalize_run_guards_the_registry() {
    let mut e = page().await;
    // A registry interface whose rows nothing else re-defines: `DOMParser.parseFromString` is
    // `required 2` in the capture-derived registry, so the pass has to have wrapped it.
    assert_eq!(
        probe(&mut e, "new DOMParser().parseFromString()"),
        "throw:TypeError:Failed to execute 'parseFromString' on 'DOMParser': 2 arguments required, but only 0 present.",
    );
    // ...and a member with no required argument is NOT wrapped: an extra guard breaks pages, which is
    // the worse half of getting this pass wrong.
    assert_eq!(probe(&mut e, "Object.prototype.toString.call(document.createRange())"), "ok:[object Range]");
}

// -- 2. the second run: the members the bootstrap finishes AFTER the first ------------------------

/// The five members that shipped unguarded from an earlier port until the status ledger found them. Each is
/// assigned onto its prototype AFTER `install_window_postshells` has run -- `CSSStyleSheet.replace` and
/// `replaceSync` by `BOOTSTRAP`'s CSSOM block, and -- until a later change -- `MediaQueryList.addListener`/
/// `removeListener` by `host.js`'s `__wireMQL` at INSTALL_DOCUMENT time, so only the SECOND run could
/// guard those. The two `MediaQueryList` cases are kept because the GUARD must hold whoever supplies
/// it: they are the table's own `required 1` rows now, and the message is unchanged. Chrome's
/// message is reproduced verbatim, pluralisation and trailing period included, because a page reads
/// `err.message`.
#[tokio::test]
async fn the_second_finalize_run_guards_what_the_bootstrap_finishes() {
    let mut e = page().await;
    for (expr, iface, member) in [
        ("new CSSStyleSheet().replaceSync()", "CSSStyleSheet", "replaceSync"),
        ("matchMedia('(min-width: 1px)').addListener()", "MediaQueryList", "addListener"),
        ("matchMedia('(min-width: 1px)').removeListener()", "MediaQueryList", "removeListener"),
        ("document.styleSheets.item()", "StyleSheetList", "item"),
    ] {
        assert_eq!(
            probe(&mut e, expr),
            format!("throw:TypeError:Failed to execute '{member}' on '{iface}': 1 argument required, but only 0 present."),
            "{iface}.{member} lost its arity guard -- the second finalize run did not reach it"
        );
    }
    // `CSSStyleSheet.replace` IS GUARDED TOO -- and it REJECTS, because it returns a promise.
    //
    // This row used to sit in the loop above demanding a synchronous throw, which asserted that
    // korvus must DIFFER from the browser: WebIDL turns a TypeError raised while invoking a
    // promise-returning operation into a rejection, and Chrome 150.0.7871.46 (`--headless=new
    // --dump-dom`, loopback, 2026-09-23, AWAITING the promise) answers
    //
    //     new CSSStyleSheet().replace()      reject TypeError: Failed to execute 'replace' on
    //                                        'CSSStyleSheet': 1 argument required, but only 0 present.
    //     new CSSStyleSheet().replaceSync()  THROW  the same sentence -- `replaceSync` is not a
    //                                        promise, and it stays in the loop above as the control
    //
    // Same guard, same sentence, different DELIVERY -- so the assertion is that it rejects, not that
    // it throws, and a promise that RESOLVES fails it just as a throw does (ARITY1; TY1 corrected
    // three live tests of exactly this shape).
    assert_eq!(probe(&mut e, "Object.prototype.toString.call(new CSSStyleSheet().replace())"), "ok:[object Promise]");
    let settled = r#"(() => {
        globalThis.__l18replace = "pending";
        new CSSStyleSheet().replace().then(
            v  => { globalThis.__l18replace = "resolve:" + Object.prototype.toString.call(v); },
            err => { globalThis.__l18replace = "reject:" + err.constructor.name + ":" + err.message; });
        return "armed";
    })()"#;
    assert_eq!(probe(&mut e, settled), "ok:armed");
    e.pump(std::time::Duration::from_millis(50)).await.expect("pump");
    assert_eq!(
        probe(&mut e, "globalThis.__l18replace"),
        "ok:reject:TypeError:Failed to execute 'replace' on 'CSSStyleSheet': 1 argument required, but only 0 present.",
        "CSSStyleSheet.replace must REJECT its arity refusal with Chrome's sentence, not throw it and not resolve"
    );
    // ...and each is still the member it was: the guard forwards, so a real call still works.
    assert_eq!(probe(&mut e, "typeof matchMedia('(min-width: 1px)').addListener(function () {})"), "ok:undefined");
    assert_eq!(probe(&mut e, "Object.prototype.toString.call(document.styleSheets.item(0))"), "ok:[object CSSStyleSheet]");
}

/// The guard is a real native function, not a JS wrapper, on both runs: `[native code]`, the member's
/// own name, its recorded length, and no own `prototype`/`arguments`/`caller`. Those are the stealth
/// axes `coverage.sh` grades, and a guard is the one thing in the tree that re-mints a shipped member.
#[tokio::test]
async fn a_second_run_guard_is_still_a_native_function() {
    let mut e = page().await;
    let src = r#"(() => {
        const f = CSSStyleSheet.prototype.replaceSync;
        return [/\{\s*\[native code\]\s*\}/.test(Function.prototype.toString.call(f)),
                f.name, f.length,
                Object.getOwnPropertyNames(f).filter(n => n !== "name" && n !== "length").join(",")].join("|");
    })()"#;
    assert_eq!(str_of(&mut e, "l18-native", src.to_string()), "true|replaceSync|1|");
}

// -- 3. the recorded enumeration order, at the end of bootstrap -----------------------------------

/// Every interface named here is a REGISTRY interface (no `interface!` table owns it), so its
/// prototype's order is the order pass's work and nothing else's. The assertion is against the
/// BUNDLE, which is the single source for what the order should be.
const ORDERED: &[&str] = &[
    "XPathEvaluator",
    "CSSStyleSheet",
    "MediaQueryList",
    "TextEncoder",
    "TextDecoder",
    "Highlight",
    "HighlightRegistry",
    "SVGStringList",
    "StylePropertyMapReadOnly",
    "ViewTransition",
    "ViewTransitionTypeSet",
    "ClipboardItem",
];

#[tokio::test]
async fn the_recorded_enumeration_order_holds_at_the_end_of_bootstrap() {
    let mut e = page().await;
    let fx = bundle();
    for iface in ORDERED {
        let recorded: Vec<String> = fx["interfaces"][iface]["members"]
            .as_array()
            .unwrap_or_else(|| panic!("{iface} is absent from the capture"))
            .iter()
            .map(|m| m["name"].as_str().unwrap().to_string())
            .collect();
        assert!(recorded.len() >= 2, "{iface} has too few recorded members to order");
        // The pass MOVES shared members and creates none, so the contract is: the members we have and
        // the capture records appear in the capture's relative order. A member we do not expose yet
        // (an honest stub row that is absent) simply drops out of both sides.
        let want: Vec<String> = {
            let src = format!(
                r#"(() => {{ const own = Object.getOwnPropertyNames({iface}.prototype);
                             return own.join(","); }})()"#
            );
            let own = str_of(&mut e, "l18-order", src);
            let own: Vec<&str> = own.split(',').filter(|s| !s.is_empty()).collect();
            recorded.iter().filter(|n| own.contains(&n.as_str())).cloned().collect()
        };
        let src = format!(
            r#"(() => {{ const rec = {};
                         const own = Object.getOwnPropertyNames({iface}.prototype);
                         return own.filter(n => rec.includes(n)).join(","); }})()"#,
            serde_json::to_string(&recorded).unwrap()
        );
        let got = str_of(&mut e, "l18-order", src);
        assert_eq!(
            got,
            want.join(","),
            "{iface}.prototype is not in the capture's enumeration order -- the order pass did not run behind the last writer"
        );
    }
}

// -- 3b. the OffscreenCanvasRenderingContext2D borrow ---------------------------------------------

/// The last block of `dom/09-postshells.js` to become Rust (`canvas_2d::borrow_offscreen_2d`).
/// `OffscreenCanvasRenderingContext2D` shares almost its whole surface with
/// `CanvasRenderingContext2D`, and only the latter has an implementation, so each shared member's
/// BODY is taken from the donor.
///
/// IT USED TO ASSERT THE SAME FUNCTION OBJECT, AND CHROME SAYS OTHERWISE. Measured on Chrome
/// 150.0.7871.46 (`--headless=new --dump-dom`), asked when the installer-level receiver
/// check turned this red:
///
/// ```text
///   CanvasRenderingContext2D.prototype.fillRect === OffscreenCanvasRenderingContext2D
///       .prototype.fillRect                                              false
///   the same for the `lineWidth` setter                                  false
///   CanvasRenderingContext2D.prototype.fillRect.call(offscreenCtx, …)    TypeError: Illegal invocation
///   OffscreenCanvasRenderingContext2D.prototype.fillRect.call(ctx, …)    TypeError: Illegal invocation
///   CanvasRenderingContext2D.prototype.isPrototypeOf(offscreenCtx)       false
/// ```
///
/// So the two prototypes carry DIFFERENT function objects, each refusing the other interface's
/// receiver, and the identity this test pinned was the engine's shortcut rather than the browser's
/// shape. The offscreen context's shared members are its own rows now, each checked against the
/// offscreen prototype (the copy and its re-aim, `install::receiver::rebind`, were deleted),
/// which reproduces all five lines above. What is still
/// asserted -- and is what "borrows" ever meant -- is that every shared member gets a WORKING body
/// rather than a D2 stub, proved by CALLING them on a real offscreen context.
#[tokio::test]
async fn the_offscreen_2d_context_borrows_the_implemented_descriptors() {
    let mut e = page().await;
    let src = r#"(() => {
        const SRC = CanvasRenderingContext2D.prototype, DST = OffscreenCanvasRenderingContext2D.prototype;
        let shared = 0, borrowed = 0, missing = [];
        for (const n of Object.getOwnPropertyNames(DST)) {
            if (n === "constructor" || n === "canvas") continue;
            const s = Object.getOwnPropertyDescriptor(SRC, n);
            if (!s) continue;                 // offscreen-only member: nothing to borrow
            shared++;
            const d = Object.getOwnPropertyDescriptor(DST, n);
            // A member that was BORROWED has the donor's shape: the same kind of descriptor and, for
            // a method, the same `length`. It is not the same function OBJECT -- the receiver check
            // is re-aimed at this prototype as it is copied, and Chrome's two are `!==` too -- so
            // identity cannot be the test. A member that was NOT borrowed is a D2 stub with no donor
            // behind it, which the CALLS below discriminate for real.
            const kindOk = (!!s.get === !!d.get) && (!!s.set === !!d.set) && (!!s.value === !!d.value);
            const lenOk = !s.value || !d.value || s.value.length === d.value.length;
            if (kindOk && lenOk) borrowed++; else missing.push(n);
        }
        return [shared > 50, borrowed === shared, missing.slice(0, 5).join(",")].join("|");
    })()"#;
    assert_eq!(
        str_of(&mut e, "l18-offscreen", src.to_string()),
        "true|true|",
        "a shared OffscreenCanvasRenderingContext2D member did not get the donor's body"
    );
    // THE ASSERTION THAT MATTERS: the borrowed bodies RUN, on a REAL offscreen 2D context. Before the
    // borrow these answered `undefined` -- 72 members and 269 recorded cases for a prototype nothing
    // wore. The receiver is an `OffscreenCanvas` context and not a window one, because that is the
    // only receiver this interface's members have in a browser: Chrome 150.0.7871.46 answers
    // `TypeError: Illegal invocation` to
    // `OffscreenCanvasRenderingContext2D.prototype.fillRect.call(windowCtx, …)`, measured.
    let src = r#"(() => {
        const ctx = new OffscreenCanvas(8, 8).getContext("2d");
        const P = OffscreenCanvasRenderingContext2D.prototype;
        const out = [];
        out.push(Object.prototype.toString.call(P.measureText.call(ctx, "x")));
        out.push(Object.prototype.toString.call(P.createLinearGradient.call(ctx, 0, 0, 1, 1)));
        const d = Object.getOwnPropertyDescriptor(P, "lineWidth");
        d.set.call(ctx, 7);
        out.push(d.get.call(ctx));
        // ...and the CROSS-receiver calls Chrome refuses are refused here too, both ways round.
        const no = f => { try { f(); return "NO-THROW"; } catch (e) { return e.name + ":" + e.message; } };
        const wctx = document.createElement("canvas").getContext("2d");
        out.push(no(() => P.fillRect.call(wctx, 0, 0, 1, 1)));
        out.push(no(() => CanvasRenderingContext2D.prototype.fillRect.call(ctx, 0, 0, 1, 1)));
        return out.join("|");
    })()"#;
    assert_eq!(
        str_of(&mut e, "l18-offscreen-run", src.to_string()),
        "[object TextMetrics]|[object CanvasGradient]|7|TypeError:Illegal invocation|TypeError:Illegal invocation",
        "a borrowed OffscreenCanvasRenderingContext2D member does not actually run, or the two \
         interfaces stopped refusing each other's receivers the way Chrome 150.0.7871.46 does"
    );
    // The CAPTURE is the denominator: the borrow iterates the DESTINATION, so a member the donor has
    // and the capture does not must NOT appear. Asserted against the bundle.
    let fx = bundle();
    let recorded: Vec<&str> = fx["interfaces"]["OffscreenCanvasRenderingContext2D"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    let got = str_of(
        &mut e,
        "l18-offscreen-names",
        r#"(() => Object.getOwnPropertyNames(OffscreenCanvasRenderingContext2D.prototype)
             .filter(n => n !== "constructor").join(","))()"#
            .to_string(),
    );
    assert_eq!(got.split(',').collect::<Vec<_>>(), recorded, "the offscreen prototype is not the capture's member list");
    // ...and `canvas` is NOT borrowed: each interface's answers a different kind of object.
    assert_eq!(
        str_of(
            &mut e,
            "l18-offscreen-canvas",
            r#"(() => String(Object.getOwnPropertyDescriptor(CanvasRenderingContext2D.prototype, "canvas").get
                             === Object.getOwnPropertyDescriptor(OffscreenCanvasRenderingContext2D.prototype, "canvas").get))()"#
                .to_string()
        ),
        "false"
    );
}

// -- 4. the constant seal -------------------------------------------------------------------------

/// The third pass in the same call: a WebIDL constant is `{writable:false, enumerable:true,
/// configurable:false}` on both the interface object and its prototype. It runs after the sort
/// because a non-configurable member cannot be moved.
#[tokio::test]
async fn the_constant_seal_holds_at_the_end_of_bootstrap() {
    let mut e = page().await;
    let src = r#"(() => {
        const out = [];
        for (const [iface, name] of [["Node", "ELEMENT_NODE"], ["Event", "CAPTURING_PHASE"],
                                     ["XPathResult", "NUMBER_TYPE"], ["NodeFilter", "SHOW_ELEMENT"]]) {
            const C = globalThis[iface];
            if (!C) { out.push(iface + ":absent"); continue; }
            for (const holder of [["obj", C], ["proto", C.prototype]]) {
                const d = holder[1] ? Object.getOwnPropertyDescriptor(holder[1], name) : null;
                out.push(iface + "." + holder[0] + "=" + (d ? `${+!!d.writable}${+!!d.enumerable}${+!!d.configurable}` : "none"));
            }
        }
        return out.join(" ");
    })()"#;
    // Recorded from `google-chrome --headless=new --dump-dom` over a loopback page, 2026-09-11:
    // every one of these is `{writable:false, enumerable:true, configurable:false}` on BOTH the
    // interface object and the prototype -- except `NodeFilter`, which is a WebIDL namespace-like
    // callback interface with no `.prototype` at all, so only the object side exists.
    assert_eq!(
        str_of(&mut e, "l18-seal", src.to_string()),
        "Node.obj=010 Node.proto=010 Event.obj=010 Event.proto=010 \
         XPathResult.obj=010 XPathResult.proto=010 NodeFilter.obj=010 NodeFilter.proto=none",
    );
}

// -- 5. the 24 Window own-globals the deleted `WINQ` table used to re-guard ------------------------

/// `install/generated_order.rs` held a second table, `WINQ`, listing 24 Window own-globals
/// and their WebIDL required-argument counts, and `install_arity_guards` (itself deleted)
/// re-applied it to whatever owned each slot on both finalize runs. It was a SECOND table about members the subsystems that
/// install them already describe, and every one of those subsystems was already counting its own
/// arguments — `fetch`, `atob`/`btoa` and `structuredClone` said so out loud by stamping
/// `stamp_arity_guarded` to opt OUT of the pass, and the rest (the `Window` table's own rows,
/// `BOOTSTRAP`'s timers, `host.js`'s frame/idle callbacks, `getComputedStyle`, `matchMedia`) simply
/// threw the identical sentence one frame further in.
///
/// This is the read-back the deletion owes: every global that had a count still throws it.
///
/// Recorded from `google-chrome --headless=new --dump-dom` over a `file://` page, 2026-09-12:
/// `getDigitalGoodsService`, `navigate` and `setResizable` are absent from Chrome too, and `fetch`
/// and `createImageBitmap` are promise-returning, so a bare call REJECTS rather than throwing —
/// which is why a generic arity guard around them would have been the right sentence in the wrong
/// shape.
const WINQ_GLOBALS: &[(&str, u32)] = &[
    ("atob", 1),
    ("btoa", 1),
    ("cancelAnimationFrame", 1),
    ("cancelIdleCallback", 1),
    // `fetchLater` was in WINQ_ABSENT until the window had one. Its arity error is
    // WebIDL's own and is the sentence Brave 151 answers, measured.
    ("fetchLater", 1),
    ("getComputedStyle", 1),
    ("matchMedia", 1),
    ("moveBy", 2),
    ("moveTo", 2),
    ("postMessage", 1),
    ("queueMicrotask", 1),
    ("reportError", 1),
    ("requestAnimationFrame", 1),
    ("requestIdleCallback", 1),
    ("resizeBy", 2),
    ("resizeTo", 2),
    ("setInterval", 1),
    ("setTimeout", 1),
    ("structuredClone", 1),
];

/// The `WINQ` names this engine does not install at all -- and all three are absent from the Chrome
/// measured above as well, so the list is now a claim about the FAMILY rather than a gap ledger.
/// Listed so the deletion cannot be mistaken for having removed a member. (`createImageBitmap` was
/// the fourth until the window got one -- `tests/canvas_draw_image_live.rs` asserts it
/// like Chrome; `fetchLater` was the fifth until it did too, and it is in `WINQ_GLOBALS` now.)
const WINQ_ABSENT: &[&str] = &["getDigitalGoodsService", "navigate", "setResizable"];

#[tokio::test]
async fn every_window_global_that_had_a_winq_count_still_throws_it() {
    let mut e = page().await;
    for (name, need) in WINQ_GLOBALS {
        let plural = if *need == 1 { "argument" } else { "arguments" };
        assert_eq!(
            probe(&mut e, &format!("globalThis.{name}()")),
            format!("throw:TypeError:Failed to execute '{name}' on 'Window': {need} {plural} required, but only 0 present."),
            "window.{name} lost its WebIDL argument count when WINQ was deleted"
        );
    }
    for name in WINQ_ABSENT {
        assert_eq!(probe(&mut e, &format!("typeof globalThis.{name}")), "ok:undefined", "{name} is suddenly installed");
    }
    // `fetch` is promise-returning, so a bare call REJECTS -- Chrome answers `[object Promise]`, not a
    // throw. It is in `WINQ` all the same, which is why `fetch_global.rs` had to stamp itself out of
    // the pass; with the pass gone the stamp is just the truthful record that the body counts its own
    // arguments.
    assert_eq!(probe(&mut e, "Object.prototype.toString.call(globalThis.fetch())"), "ok:[object Promise]");
}

/// Chrome checks the RECEIVER before the argument count, measured the same day:
///
/// ```text
///   window.moveBy()             TypeError: Failed to execute 'moveBy' on 'Window': 2 arguments required, …
///   const f = window.moveBy; f()   the same sentence   (sloppy-mode `this` is the window)
///   f.call({})                  TypeError: Illegal invocation
/// ```
///
/// The `Window` table already builds each of these six rows as an arity guard wrapped in a receiver
/// guard, which is exactly that order. `WINQ` then wrapped a SECOND arity guard around the pair from
/// the OUTSIDE -- `window.moveBy.call({})` answered the argument-count sentence where Chrome answers
/// `Illegal invocation` -- because `window.rs`'s `guarded()` did not carry the arity brand the way
/// `install::receiver`'s wrapper does. Deleting the table fixes the order; this pins it.
#[tokio::test]
async fn a_window_table_global_checks_its_receiver_before_its_argument_count() {
    let mut e = page().await;
    for name in ["moveBy", "moveTo", "resizeBy", "resizeTo", "postMessage", "reportError"] {
        assert_eq!(
            probe(&mut e, &format!("globalThis.{name}.call({{}})")),
            "throw:TypeError:Illegal invocation",
            "window.{name} answered its argument count to a foreign receiver -- Chrome answers Illegal invocation"
        );
    }
}
