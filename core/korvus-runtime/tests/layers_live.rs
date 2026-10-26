//! LIVE gate for the exact `layer` ledger (goal invariant I7, track W7).
//!
//! WHAT WENT WRONG, AND WHY A TEST NOW STANDS HERE. `tools/status/data/status.json`'s `layer` field was read
//! off the runtime SHELL INVENTORY -- the labels `make_shell_method` pushed when it built a shape-only
//! member. `install/stub.rs`, the D2 stub that every HAND table's `= stub` row mounts, registered
//! nothing into that inventory, so a hand-table stub reported `layer: "implemented"`. The field was
//! wrong in the direction that FLATTERS, across hundreds of rows, and every W5.1 lane quoted it.
//!
//! The layer is a COLUMN of the row, so there are now two derivations of it and they must agree:
//!   * STATIC -- `tools/sync/gen_layers.py` reads the tables and writes `tools/status/data/layers.json`;
//!   * LIVE   -- `install::installer::put_member` (the one member mount) records the layer of every
//!     row it mounts, and `behavior_rt::layers_json()` hands the result back.
//!
//! This test boots the real engine and asserts the two agree member by member. Two derivations that
//! must agree is what makes the mechanism unable to regress quietly: change either rule alone and this
//! goes red.

use engine::Engine;

async fn page() -> Engine {
    // THE BUNDLE THIS RUN WEARS: `$KORVUS_BUNDLE` if set, else the embedded one -- not `default()`,
    // the embedded bundle whatever the environment says. Presence and order follow the ACTIVE bundle,
    // so `KORVUS_BUNDLE=<id>` has to reach this gate or it can only ever grade one device.
    let mut e = Engine::new(Box::new(
        persona::CapturedPersona::select(std::env::var("KORVUS_BUNDLE").ok().as_deref()).unwrap_or_else(|e| panic!("{e}")),
    ));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e
}

/// Every layer `tools/status/data/layers.json` declares for one member, across BOTH realms.
///
/// WHY BOTH, in a test that boots a page. The live map is process-wide, and this whole suite runs in
/// ONE binary: other tests spawn worker contexts, whose rows come from the worker registry, and the
/// two registries can carry different bodies for a name both realms expose (`ImageData`'s attributes
/// are the `slot_get` kit in the window registry and plain stubs in the worker one). So the live map
/// this test reads is a mix of both realms' answers, in an order the test harness chooses. Asserting
/// against the window realm alone made this test PASS alone and FAIL in the suite -- a flake, and the
/// wrong kind: it would have gone green again by running the file on its own.
///
/// Every value the map can hold is still SOME table's declaration for that member -- `record_layer`
/// keeps the weakest of the answers it is given, never invents one -- so "is the live layer one of
/// the layers a table declares for this member" is the exact property, and it is order-independent.
fn declared_layers(d: &serde_json::Value, iface: &str, member: &str) -> Vec<String> {
    ["window", "worker"]
        .iter()
        .filter_map(|realm| d["layers"][realm][iface][member].as_str().map(str::to_string))
        .collect()
}

fn declared() -> serde_json::Value {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/status/data/layers.json");
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))).unwrap()
}

fn live() -> serde_json::Map<String, serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(&webapi::behavior_rt::layers_json()).unwrap().as_object().unwrap().clone()
}

#[tokio::test]
async fn the_live_layer_of_every_mounted_member_is_the_layer_its_table_declares() {
    let _e = page().await;
    let (declared, live) = (declared(), live());
    assert!(live.len() > 1000, "the live layer map is nearly empty ({}) -- put_member stopped recording", live.len());

    let mut disagree: Vec<String> = Vec::new();
    let mut unmounted_cppgc = 0usize;
    let mut matched = 0usize;
    for (label, got) in &live {
        let got = got.as_str().unwrap();
        let (iface, member) = match label.split_once("::") {
            Some(p) => p,
            None => continue,
        };
        let want = declared_layers(&declared, iface, member);
        if want.is_empty() {
            // A member the installer mounted from a table `gen_layers.py` did not see is a parser
            // gap, not a tolerance: every mounted row comes from a table.
            disagree.push(format!("{label}: mounted {got}, but tools/status/data/layers.json has no row"));
            continue;
        }
        if want.iter().any(|w| w == got) {
            matched += 1;
            continue;
        }
        // A `= cppgc` row says "a cppgc class template realises this member". When the installer is
        // minting the prototype itself no template did, so it mounts the D2 stub and honestly records
        // `stub`; the static file cannot know which of the two happened. No row is in that state
        // today (the counter below reads 0) -- it is tolerated, not expected, and reported either way.
        if want.iter().all(|w| w == "cppgc") && got == "stub" {
            unmounted_cppgc += 1;
            continue;
        }
        disagree.push(format!("{label}: table declares {want:?}, installer mounted {got}"));
    }
    assert!(matched > 1000, "only {matched} members agreed -- the join is not working");
    assert!(
        disagree.is_empty(),
        "{} member(s) disagree between tools/status/data/layers.json and the live mount; run \
         scripts/sync.sh, or fix whichever of gen_layers.py / installer::mounted_layer is wrong:\n{}",
        disagree.len(),
        disagree.iter().take(25).cloned().collect::<Vec<_>>().join("\n")
    );
    eprintln!("layers: {matched} members agree, {unmounted_cppgc} `= cppgc` rows mounted as stubs");
}

#[tokio::test]
async fn a_stub_row_reports_stub_and_an_implemented_row_reports_native() {
    let _e = page().await;
    let (declared, live) = (declared(), live());

    // THE PROPERTY THIS UNIT RESTORES, stated as a test: no row the tables declare `stub` may be
    // reported as implemented. That is the exact failure the shell inventory had -- it could not see
    // the D2 stub at all, so every hand-table `= stub` row read as `implemented`.
    let mut flattered = Vec::new();
    let (mut stubs, mut natives) = (0usize, 0usize);
    for (label, got) in &live {
        let got = got.as_str().unwrap();
        let Some((iface, member)) = label.split_once("::") else { continue };
        let want = declared_layers(&declared, iface, member);
        // "Every table that declares this member declares it a stub" -- so no reading of the tables
        // can justify calling it implemented.
        if !want.is_empty() && want.iter().all(|w| w == "stub") {
            if matches!(got, "native" | "cppgc") {
                flattered.push(format!("{label}: declared stub, mounted {got}"));
            }
            stubs += 1;
        }
        if got == "native" || got == "cppgc" {
            natives += 1;
        }
    }
    assert!(flattered.is_empty(), "a declared stub is reported as implemented:\n{}", flattered.join("\n"));
    // Both halves must be non-empty, or the assertion above is vacuous.
    assert!(stubs > 100, "only {stubs} declared-stub members were mounted -- the map is not covering the stubs");
    assert!(natives > 100, "only {natives} implemented members were mounted -- the map is not covering the bodies");

    // Two named rows, so a regression that erased the distinction cannot pass by emptying one side.
    // A `= cppgc` row of an OWN class mounts its generated callback and records `native`.
    // Until then a deno_core class template realised such a row and the installer VERIFIED it, recording
    // `cppgc` -- DOMRectReadOnly's `width`, then `Headers.get`, were this assertion's row; no deno_core
    // class realises a table member any more.
    assert_eq!(webapi::behavior_rt::layer_of("Headers", "get"), Some("native"));
    assert_eq!(webapi::behavior_rt::layer_of("DOMRectReadOnly", "width"), Some("native"));
    // A registry row with no body at all: `Object` is skipped entirely (V8 owns it), so pick a member
    // the installer really mounts. `EventSource.close` is a generated-registry `= stub` row.
    // (`WebSocket.send` was the example until the WebSocket rows were claimed for a real body.)
    assert_eq!(webapi::behavior_rt::layer_of("EventSource", "close"), Some("stub"), "EventSource.close should be a stub");
}

/// `KORVUS_STUB=throw` IS OFF BY DEFAULT, proved by reaching a stub rather than by reading the parser.
///
/// The D2 contract is that an unimplemented member is observable as ABSENT -- a shape-correct empty
/// value -- and never as a crash, because an unguarded feature-detect that currently degrades would
/// start breaking pages the moment a stub threw. `KORVUS_STUB` selects `log` (same answers, plus the
/// call site) and `throw` (a `NotSupportedError`) for diagnosis; neither is the default, and this test
/// runs with the variable unset, as every page does.
#[tokio::test]
async fn a_stub_answers_empty_and_does_not_throw_by_default() {
    let mut e = page().await;
    // Two stubs a real page reaches, both `layer: "stub"` in tools/status/data/layers.json, read off REAL
    // instances (the receiver check means a stub read off the prototype is a different question).
    //
    // `Document.featurePolicy` was one of them until it got a real body
    // (`core/feature_policy/`); `caretRangeFromPoint` took its place as the second object-typed
    // stub, so the "an object-typed stub reads null" half of this test still has two witnesses.
    // `Document.all` was the OTHER one until it became a real `HTMLAllCollection` -- it is
    // now `native` in layers.json and answers an object, not `null` -- and
    // `Element.ariaActiveDescendantElement` (`attr : Object nullable = stub`, core/dom/element.rs)
    // replaced it. The WITNESS changes when a member gets a body; the RULE does not.
    let src = r#"(() => {
        const r = [];
        for (const [label, f] of [
            ["Document.caretRangeFromPoint", () => document.caretRangeFromPoint()],
            ["Element.ariaActiveDescendantElement", () => document.body.ariaActiveDescendantElement],
            ["Element.computedStyleMap", () => document.body.computedStyleMap()],
        ]) {
            try { const v = f(); r.push(`${label}=${v === null ? "null" : typeof v}`); }
            catch (err) { r.push(`${label}=THREW ${err && err.name}`); }
        }
        return r.join(" ");
    })()"#;
    let got = e.eval_value("stub-quiet", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert!(!got.contains("THREW"), "a stub threw with KORVUS_STUB unset: {got}");
    // And the answers are the SHAPE defaults the row declares, not `undefined` for everything: an
    // object-typed stub reads `null`, so `typeof` never has to be the whole story.
    assert!(got.contains("Element.ariaActiveDescendantElement=null"), "{got}");
}

/// THE D2 STUB REFUSES A FOREIGN RECEIVER, for a HAND-TABLE row and a REGISTRY row alike.
///
/// WHY THIS TEST EXISTS RATHER THAN A COMMENT. W5.1 lane L17 (`8ff28e75`) declined to transcribe
/// members into `= stub` rows because "the D2 stub has no receiver check where the shell stub it
/// replaced had one", so a transcription would have DROPPED a check and made a member answer for any
/// object. That was true when L17 wrote it and stopped being true in the same day's `c564fe58`, which
/// folded the shell stub's check into the one stub as the two became one (invariant I7) -- and a
/// reason a lane declines work on has to be checkable, or it outlives the defect and blocks lanes
/// that no longer need blocking. The note in `core/html/webaudio.rs` said so for four Web Audio
/// interfaces; it now points here.
///
/// The two mounts differ and both are covered: a REGISTRY row's receiver must carry the shell brand
/// (`shell_receiver_ok`), a HAND-TABLE row's is the prototype chain alone (`table_receiver_ok`),
/// because a table mints its own instances and they wear no shell brand.
///
/// ASKED CHROME 150.0.7871.46 (`--headless=new --dump-dom`) for every line below. It answers
/// `TypeError: Illegal invocation` to all nine -- for a bare `{}` receiver AND for a read off the
/// interface prototype itself, which is not an instance either.
///
/// EVERY ROW HERE IS ONE THE D2 STUB STILL OWNS, and that qualification is a finding rather than a
/// convenience. Sweeping all 2,399 rows `tools/status/data/layers.json` declares `stub` found 157 that
/// answer for a foreign receiver, and they split in two: ~80 are the ECMAScript intrinsics the
/// capture recorded and V8 owns (`Array.push`, `String.trim`, `Object.valueOf` -- generic in Chrome
/// too, so `layers.json` calling them `stub` is a ledger inaccuracy and not a tell), and ~77 are Web
/// API rows a SECOND WRITER overwrote after the installer mounted the stub -- `HTMLLinkElement.sheet`
/// and `Document.elementFromPoint` WERE masked JS bodies in `korvus-runtime/src/lib.rs`'s string
/// literals, and the OffscreenCanvasRenderingContext2D and Headers/FormData families were the same
/// shape. Those rows were not stubs live, whatever the table said, so they do not belong in a test
/// about what the stub does; they belonged in the W7 ledger note and in W4/W5's deletion of the
/// second writer, and both is where they went. `second_writers.manifest` is empty now and the mask
/// ops that made those bodies look native are deleted.
#[tokio::test]
async fn a_stub_row_refuses_a_foreign_receiver_in_both_mounts() {
    let mut e = page().await;
    let src = r#"(() => {
        const r = [];
        const get = (p, n) => Object.getOwnPropertyDescriptor(p, n).get;
        const cases = [
            // HAND-TABLE `= stub` rows: core/intersection_observer/intersection_observer.rs,
            // core/css/view_transition.rs, core/html/html_select_element.rs.
            //
            // `MutationRecord.previousSibling` WAS THE FIRST TWO CASES HERE and is not a stub any more
            // Its nine rows became `slot_get` when the record mint moved to Rust. It went
            // on answering `Illegal invocation` for both receivers throughout -- a `slot_get` row's
            // receiver check and a stub's agree here -- so this file never went red over it; the two
            // lines simply stopped being ABOUT a stub. `IntersectionObserverEntry.rootBounds` (the
            // nullable shape) and `ViewTransition.transitionRoot` take their places, so the nullable
            // and the prototype-receiver cases are both still covered.
            ["table attr  IntersectionObserverEntry.rootBounds   ", () => IntersectionObserverEntry.prototype.rootBounds],
            ["table attr  IntersectionObserverEntry.time foreign ", () => get(IntersectionObserverEntry.prototype, "time").call({})],
            ["table attr  ViewTransition.transitionRoot proto    ", () => ViewTransition.prototype.transitionRoot],
            ["table meth  HTMLSelectElement.item foreign         ", () => HTMLSelectElement.prototype.item.call({}, 0)],
            ["table meth  HTMLSelectElement.namedItem foreign    ", () => HTMLSelectElement.prototype.namedItem.call({}, "x")],
            // REGISTRY `= stub` rows (generated_registry.rs).
            // (`WebSocket.send` and `CloseEvent.reason` were the first and last registry cases until
            // both interfaces got real bodies; `EventSource.close` and `StorageEvent.key` are rows
            // the D2 stub still owns, and Chrome answers both the same way.)
            ["registry meth EventSource.close foreign            ", () => EventSource.prototype.close.call({})],
            ["registry attr CompositionEvent.data foreign        ", () => get(CompositionEvent.prototype, "data").call({})],
            ["registry attr CompositionEvent.data proto          ", () => CompositionEvent.prototype.data],
            ["registry attr StorageEvent.key proto               ", () => StorageEvent.prototype.key],
        ];
        for (const [label, f] of cases) {
            try { const v = f(); r.push(`${label} = NO-THROW(${v === null ? "null" : typeof v})`); }
            catch (err) { r.push(`${label} = ${err && err.name}:${err && err.message}`); }
        }
        return r.join("\n");
    })()"#;
    let got = e.eval_value("stub-foreign", src.to_string()).unwrap().as_str().unwrap().to_string();
    for line in got.lines() {
        assert!(
            line.ends_with("= TypeError:Illegal invocation"),
            "a D2 stub answered for a receiver that is not an instance -- Chrome throws \
             `TypeError: Illegal invocation` for every one of these:\n{got}"
        );
    }
    assert_eq!(got.lines().count(), 9, "every case must report:\n{got}");
}

/// A STUB ANSWERS ITS OWN KIND OF EMPTY, on a real instance, from the row's IDL type.
///
/// `CompositionEvent.data` is the worked example and the reason this test exists: W4.b found it
/// answering `undefined` where Chrome 150.0.7871.46 answers `""`, because every generated-registry
/// row carried `ty: TypeTag::Any` whatever webref declared. Each line below is a Chrome answer,
/// measured with `--headless=new --dump-dom`, not an engine answer written down.
#[tokio::test]
async fn a_stub_on_a_real_instance_answers_the_shape_its_idl_type_declares() {
    let mut e = page().await;
    let src = r#"(() => {
        const j = v => v === null ? "null" : v === undefined ? "undefined"
            : typeof v === "string" ? JSON.stringify(v)
            : Array.isArray(v) ? "[]"
            : typeof v === "object" ? (v instanceof Promise ? "Promise" : Object.prototype.toString.call(v))
            : String(v);
        const r = [];
        for (const [label, f] of [
            // USVString -> "" (the W4.b defect).       DOMString  -> ""
            ["CompositionEvent.data", () => new CompositionEvent("x").data],
            ["CloseEvent.reason", () => new CloseEvent("x").reason],
            // boolean -> false                          unsigned short -> 0
            ["CloseEvent.wasClean", () => new CloseEvent("x").wasClean],
            ["CloseEvent.code", () => new CloseEvent("x").code],
            // DOMString? -> null, NOT "": nullability outranks the type.
            ["StorageEvent.key", () => new StorageEvent("x").key],
            // USVString (not nullable) -> "" on the same interface, so the pair proves the rule.
            ["StorageEvent.url", () => new StorageEvent("x").url],
            // Promise<...> -> an already-resolved promise, so `.then` works instead of throwing.
            ["PublicKeyCredential.isConditionalMediationAvailable",
             () => PublicKeyCredential.isConditionalMediationAvailable()],
        ]) {
            try { r.push(`${label}=${j(f())}`); } catch (err) { r.push(`${label}=THREW ${err && err.name}`); }
        }
        return r.join(" ");
    })()"#;
    let got = e.eval_value("stub-shape", src.to_string()).unwrap().as_str().unwrap().to_string();
    for want in [
        r#"CompositionEvent.data="""#,
        r#"CloseEvent.reason="""#,
        "CloseEvent.wasClean=false",
        "CloseEvent.code=0",
        "StorageEvent.key=null",
        r#"StorageEvent.url="""#,
        "PublicKeyCredential.isConditionalMediationAvailable=Promise",
    ] {
        assert!(got.contains(want), "expected `{want}` (Chrome 150's answer) in:\n{got}");
    }
}
