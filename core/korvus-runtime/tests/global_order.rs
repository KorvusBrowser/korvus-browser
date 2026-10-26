//! WINDOW own-global ORDER against the capture bundle.
//!
//! `Object.getOwnPropertyNames(globalThis)` is a SEQUENCE, and a page can read all of it in one call.
//! Every other global-scope gate in this tree compares sets — `quality::scan_globals` reports
//! `extra_globals` / `missing_globals`, `tests/automation_tells.rs` looks for specific names — and a set
//! comparison is satisfied by a realm that exposes exactly the right names in exactly the wrong order.
//! Nothing measured the order until this file. It had been measured twice in passing this session, by
//! units that reverted their probes, which made the number folklore: true, unowned, and one merge away
//! from being wrong with nobody noticing.
//!
//! The metric lives in `webapi_gen::order`, not here, because the WORKER realm's gate
//! (`korvus-webapi/tests/worker_surface.rs`) measures the same property and two copies of a metric is
//! two truths about one fact. Read that module's header for why the gate ratchets `out_of_place`
//! (common − longest-common-relative-order) and only PRINTS the positions-agree count; its unit tests
//! pin what both numbers do under a reorder, an insertion, a removal and a relocation.
//!
//! THE BUNDLE IS THE DENOMINATOR -- the WORN one, and every one of them. Each bundle in the tree is
//! worn in turn and graded against its own `surface.json`, read through the persona, so the baseline
//! moves with the engine instead of scoring it against a browser it stopped imitating
//! (it graded only `$KORVUS_BUNDLE`'s, which the gate never sets).

use engine::Engine;

/// Every bundle directory that carries a manifest, sorted. The denominator is the tree, not a list --
/// the same loop `persona_surface.rs` walks, so a capture dropped into `bundles/` is graded here too.
///
/// WHY THIS GATE LOOPS. It used to grade `$KORVUS_BUNDLE`'s persona, and the gate only
/// ever ran the default one: under `edge-151` and `opera-135` this ratchet stood at 533 and 532
/// against a ceiling of 531 -- red, and invisible, for as long as nobody set the variable by hand.
/// A persona the engine can wear is a persona this gate grades.
fn bundles() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/korvus-persona/bundles");
    let mut out: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("manifest.toml").is_file())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no persona bundle in the tree");
    out
}

fn persona_for(id: &str) -> persona::CapturedPersona {
    persona::CapturedPersona::select(Some(id)).unwrap_or_else(|e| panic!("select bundle {id}: {e}"))
}

/// The window realm's own-global names, in the order V8 hands them to a page, wearing bundle `id`.
///
/// `load_html` + a full `run()` first, because half the window's globals are installed during the ESM
/// layer and a realm read before it settles is a realm no page ever sees.
async fn observed_window_globals(id: &str) -> Vec<String> {
    let mut e = Engine::new(persona::worn_on_host(persona_for(id)));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><head><title>t</title></head><body>x</body></html>").unwrap();
    e.run().await.unwrap();
    let raw = e
        .eval_value("global-order", "JSON.stringify(Object.getOwnPropertyNames(globalThis))".into())
        .unwrap();
    serde_json::from_str(raw.as_str().expect("the probe returns a JSON string")).expect("a JSON array of names")
}

/// Bundle `id`'s recorded window globals, in the captured browser's order.
fn baseline_window_globals(id: &str) -> Vec<String> {
    let path = persona_for(id).artifact("surface");
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display())),
    )
    .expect("the bundle's surface.json is valid JSON");
    webapi_gen::order::globals_of(&v)
}

#[tokio::test]
async fn window_own_global_order_matches_the_bundle() {
    // Every bundle is measured before any is failed, so one red persona cannot hide the others' numbers.
    let mut worst = 0usize;
    let mut red = Vec::new();
    for id in bundles() {
        match window_order_under(&id).await {
            Ok(n) => worst = worst.max(n),
            Err(msg) => red.push(msg),
        }
    }
    println!("WINDOW own-global out_of_place, worst over the green bundles: {worst}");
    assert!(red.is_empty(), "{}", red.join("\n\n"));
}

/// One bundle's window order, graded against the ceiling: its `out_of_place`, or the failure.
async fn window_order_under(id: &str) -> Result<usize, String> {
    let observed = observed_window_globals(id).await;
    let baseline = baseline_window_globals(id);
    let obs: Vec<&str> = observed.iter().map(String::as_str).collect();
    let base: Vec<&str> = baseline.iter().map(String::as_str).collect();
    let order = webapi_gen::order::compare(&base, &obs);
    println!("── bundle {id}");
    print!("{}", order.render("WINDOW", 60));
    // A count says how bad, and the witness list says which names; neither lets you see the SHAPE of
    // the difference — whether a region is reversed, or sorted, or simply installed as a block in the
    // wrong place. That needs the whole sequence beside the bundle's, so a unit fixing the order can
    // ask for it. Same shape as `KORVUS_SNAPSHOT_OUT` in the quality harness. One file per bundle:
    // `<KORVUS_GLOBAL_ORDER_OUT>.<bundle id>.json`.
    if let Ok(out) = std::env::var("KORVUS_GLOBAL_ORDER_OUT") {
        let out = format!("{out}.{id}.json");
        std::fs::write(&out, serde_json::to_string_pretty(&observed).unwrap())
            .unwrap_or_else(|e| panic!("KORVUS_GLOBAL_ORDER_OUT write to {out} failed: {e}"));
        eprintln!("wrote the window's own-global order -> {out}");
    }

    // NON-VACUITY BEFORE THE RATCHET. An empty intersection scores a perfect zero, so the denominator is
    // asserted first — this is the `present.len() >= 10` failure mode that let a 292/294 worker-surface
    // regression print itself and pass, and it is cheap to not repeat.
    assert!(
        order.common_len >= 1000,
        "[{id}] the window/bundle global intersection collapsed to {} names (baseline {}, observed {}); \
         the order number below is meaningless until that is explained",
        order.common_len, order.baseline_len, order.observed_len
    );

    // THE RATCHET, AT ZERO, UNDER EVERY BUNDLE.
    //
    // History, because the number is only meaningful beside it. Measured 2026-09-12 at e93dabf7 under
    // the default bundle: 1161 common names, LCRO 630, 531 out of place, 61 positions agreeing. At
    // `fd6473b8` under all four: brave-151 531 (1163 common), chrome-150 531 (1180), edge-151 533
    // (1183), opera-135 532 (1180) -- the last two RED against 531 and invisible, because this test
    // graded only `$KORVUS_BUNDLE`'s persona and the gate never sets it. It loops now.
    //
    // W10.3 measured the fix and did not apply it: re-sequencing the engine-created names into the
    // bundle's `globals` order took the replay 531 -> 7, leaving `Temporal`, `SuppressedError`,
    // `DisposableStack`, `AsyncDisposableStack`, `Float16Array`, `WebAssembly` and `queueMicrotask`,
    // which V8 and deno_core create before any install and which it called irreducible "by
    // re-sequencing". They are not irreducible: P12f's `installer::seat_globals_in_capture_order` --
    // the registry walk's last step, over the same `globals` sequence the walk creates from -- DELETES
    // and re-DEFINES each own global from the first divergence on, with the descriptor it had, and V8
    // gives a re-defined global property a new enumeration index. The value is the same object and
    // all four attributes are copied (measured: every one of the 1,163 brave-151 globals reads the
    // same `configurable`/kind/`enumerable`/`writable` before and after), so the move is
    // unobservable except as the order. The five `[LegacyUnforgeable]`/`chrome` names, which cannot
    // be deleted once non-configurable, are defined configurable and locked by the seat at their
    // position (`installer::lock_at_seat`) -- see `the_seat_keeps_the_unforgeable_globals_unforgeable`.
    //
    // Result: 0 out of place under all four bundles, window and worker. The residue is EMPTY. What
    // this ratchet cannot see is the 39..44 names each bundle has and the realm lacks -- those are
    // presence, not order, and `persona_surface.rs` counts them.
    //
    // A CEILING: it may fall, never rise -- and it cannot fall further.
    const WINDOW_GLOBALS_OUT_OF_PLACE: usize = 0;
    if order.out_of_place > WINDOW_GLOBALS_OUT_OF_PLACE {
        return Err(format!(
            "[{id}] window own-global ORDER regressed: {} names out of place (ceiling {WINDOW_GLOBALS_OUT_OF_PLACE}). \
             The sequence a page reads got further from the bundle's.\n{}",
            order.out_of_place,
            order.render("WINDOW", 80)
        ));
    }
    Ok(order.out_of_place)
}

/// The measurement must be STABLE: the same tree read twice gives the same number, or the ratchet is
/// noise with a ceiling on it. Two independent engines in one test, because a realm assembled twice is
/// where an ordering that depends on hash iteration or on task scheduling would show itself — and a
/// metric whose value moves between two runs of one binary cannot be a gate.
#[tokio::test]
async fn the_window_order_measurement_is_stable_across_two_boots() {
    let id = bundles().into_iter().next().expect("a bundle");
    let base_owned = baseline_window_globals(&id);
    let base: Vec<&str> = base_owned.iter().map(String::as_str).collect();

    let first_owned = observed_window_globals(&id).await;
    let second_owned = observed_window_globals(&id).await;
    assert_eq!(
        first_owned, second_owned,
        "two boots of the same tree produced different own-global orders; the realm is not deterministic"
    );

    let first: Vec<&str> = first_owned.iter().map(String::as_str).collect();
    let second: Vec<&str> = second_owned.iter().map(String::as_str).collect();
    let (a, b) = (webapi_gen::order::compare(&base, &first), webapi_gen::order::compare(&base, &second));
    assert_eq!((a.out_of_place, a.lcro, a.positions_agree), (b.out_of_place, b.lcro, b.positions_agree));
    assert_eq!(a.displaced, b.displaced, "the witness set is not reproducible");
}

/// Seating a global DELETES it and defines it again, which a non-configurable property refuses. The
/// four that must END non-configurable -- `window`, `document`, `location` and `top`, `[LegacyUnforgeable]`
/// -- are therefore created configurable and made non-configurable by the seat itself. This pins that
/// the lock took, in every bundle. `chrome` is Chrome 152's `{writable, enumerable, configurable: true}`
/// (`modules/chrome_object.rs`; Chrome 150's was configurable: false): seated, never locked. And the V8
/// staged builtins it moved keep the ECMAScript global-property shape `{writable: true, enumerable:
/// false, configurable: true}`. Every expected row below is what the stock browser answers, in
/// `tests/fixtures/p11a/chrome-152.desc.txt`.
#[tokio::test]
async fn the_seat_keeps_the_unforgeable_globals_unforgeable() {
    const PROBE: &str = r#"JSON.stringify(["window", "document", "location", "top", "chrome", "Temporal", "WebAssembly", "Float16Array", "queueMicrotask"].map(n => {
        const d = Object.getOwnPropertyDescriptor(globalThis, n);
        return d ? [n, d.configurable, d.enumerable, "value" in d ? d.writable : "accessor"] : [n, "absent"];
    }))"#;
    for id in bundles() {
        let mut e = Engine::new(persona::worn_on_host(persona_for(&id)));
        e.run().await.unwrap();
        e.load_html("<!doctype html><html><head><title>t</title></head><body>x</body></html>").unwrap();
        e.run().await.unwrap();
        let raw = e.eval_value("seat-descriptors", PROBE.into()).unwrap();
        let got: Vec<serde_json::Value> = serde_json::from_str(raw.as_str().unwrap()).unwrap();
        let want = serde_json::json!([
            ["window", false, true, "accessor"],
            ["document", false, true, "accessor"],
            ["location", false, true, "accessor"],
            ["top", false, true, "accessor"],
            ["chrome", true, true, true],
            ["Temporal", true, false, true],
            ["WebAssembly", true, false, true],
            ["Float16Array", true, false, true],
            ["queueMicrotask", true, true, true],
        ]);
        assert_eq!(serde_json::Value::Array(got), want, "[{id}] a seated global lost its descriptor");
    }
}
