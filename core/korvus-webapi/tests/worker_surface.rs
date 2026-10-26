//! WorkerGlobalScope surface parity. Dumps the surface a fingerprinter sees INSIDE a real
//! Korvus worker isolate (surface-dump.js run in the worker, result posted back) and diffs it against the
//! captured real-Chrome worker baseline (`fixtures/brave-151.worker.surface.json`). Same machinery and
//! metric set as the window-surface grading in the unified `quality` harness (its sibling here), but
//! the dump comes from the worker scope, not the window.
//!
//! The baseline was captured on https://example.com (a SECURE context) so it includes the
//! [SecureContext] worker APIs (crypto.subtle / SubtleCrypto / CryptoKey / Cache / CacheStorage) that a
//! real fingerprinter on an https site sees -- matching how the window baseline was captured.
use engine::Engine;
use std::collections::{BTreeMap, BTreeSet};


// The shared dumper (this crate's fixtures: the harness script Korvus and the capture both run) + the
// captured worker baseline (the shipped bundle's `worker-surface` artifact).
const DUMPER: &str = include_str!("fixtures/surface-dump.js");
/// Every bundle directory that carries a manifest, sorted. The denominator is the tree, not a list.
///
/// WHY THIS GATE LOOPS. It graded `$KORVUS_BUNDLE`'s persona -- `CapturedPersona::selected()`
/// -- and the gate only ever runs the default one, so a persona nobody set by hand could go red here and
/// stay invisible (the window's order ratchet did exactly that under `edge-151` and `opera-135`). Every
/// bundle the engine can wear is graded in-process, through `CapturedPersona::select(Some(id))`, which
/// is the same door `KORVUS_BUNDLE` takes with the id passed in.
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

/// Bundle `id`'s worker surface -- the device this engine is wearing, not the embedded one.
fn baseline(id: &str) -> String {
    std::fs::read_to_string(persona_for(id).artifact("worker-surface")).expect("read the bundle's worker.surface.json")
}

type Members = BTreeMap<String, serde_json::Value>; // member name -> record

fn members_of(iface: &serde_json::Value) -> Members {
    iface["members"].as_array().map(|arr| {
        arr.iter().filter_map(|m| m["name"].as_str().map(|n| (n.to_string(), m.clone()))).collect()
    }).unwrap_or_default()
}

/// Run surface-dump.js inside a real worker isolate and return the JSON string it posts back.
async fn dump_worker_surface(id: &str) -> String {
    let mut e = Engine::new(persona::worn_on_host(persona_for(id)));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    // The worker script evaluates the dumper (an IIFE returning a JSON string) and posts it back.
    let worker_src = format!("self.postMessage({DUMPER});");
    let reg = format!(
        "globalThis.__workerScripts = {{ './dump.js': {} }};",
        serde_json::to_string(&worker_src).unwrap()
    );
    e.eval("worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval(
        "dump-driver",
        "const w = new Worker('./dump.js'); w.onmessage = (ev) => print('WSURFACE:' + ev.data);".into(),
    )
    .unwrap();
    // Pump until the worker posts its surface (cross-thread).
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(line) = out.iter().find(|l| l.starts_with("WSURFACE:")) {
            return line["WSURFACE:".len()..].to_string();
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            panic!("worker did not post its surface within 20s; output: {out:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn worker_surface_parity_scoreboard() {
    for id in bundles() {
        println!("\n── bundle {id}");
        scoreboard_under(&id).await;
    }
}

/// The whole scoreboard, wearing bundle `id`. Every assertion below names no bundle; the `── bundle`
/// line printed before each run is which one a failure belongs to.
async fn scoreboard_under(id: &str) {
    let json = dump_worker_surface(id).await;
    let korvus: serde_json::Value = serde_json::from_str(&json).expect("worker surface is valid JSON");
    let brave: serde_json::Value = serde_json::from_str(&baseline(id)).unwrap();

    let k_ifaces = korvus["interfaces"].as_object().unwrap();
    let b_ifaces = brave["interfaces"].as_object().unwrap();
    let k_names: BTreeSet<&String> = k_ifaces.keys().collect();
    let b_names: BTreeSet<&String> = b_ifaces.keys().collect();

    let present: Vec<&String> = k_names.intersection(&b_names).cloned().collect();
    let missing: Vec<&String> = b_names.difference(&k_names).cloned().collect();
    let extra_ifaces: Vec<&String> = k_names.difference(&b_names).cloned().collect();

    let mut b_member_total = 0usize;
    for v in b_ifaces.values() { b_member_total += members_of(v).len(); }
    let mut k_member_total = 0usize;
    for v in k_ifaces.values() { k_member_total += members_of(v).len(); }

    let mut lies: Vec<String> = Vec::new();          // korvus non-native where brave native
    let mut extra_members: Vec<String> = Vec::new(); // korvus-only member on a shared interface (tell)
    let mut shape_mismatch: Vec<String> = Vec::new();
    let mut name_defect: Vec<String> = Vec::new();
    let mut length_defect: Vec<String> = Vec::new();
    let mut covered_members = 0usize;
    let mut ctor_name_defect: Vec<String> = Vec::new();
    let mut own_tells: Vec<String> = Vec::new();
    let mut proto_mismatch: Vec<String> = Vec::new();
    let mut tag_mismatch: Vec<String> = Vec::new();

    for name in &present {
        let ki = &k_ifaces[*name];
        let bi = &b_ifaces[*name];
        let k_ctor_name = ki["ctorName"].as_str().unwrap_or("");
        let legacy_alias = matches!((name.as_str(), k_ctor_name),
            ("WebKitCSSMatrix", "DOMMatrix") | ("WebKitMutationObserver", "MutationObserver") | ("webkitURL", "URL"));
        if !k_ctor_name.is_empty() && k_ctor_name != name.as_str() && !legacy_alias {
            ctor_name_defect.push(format!("{name} (ctor.name=\"{k_ctor_name}\")"));
        }
        if bi.get("proto").is_some() && ki.get("proto") != bi.get("proto") {
            proto_mismatch.push(name.to_string());
        }
        if bi.get("toStringTag").is_some() && ki.get("toStringTag") != bi.get("toStringTag") {
            tag_mismatch.push(name.to_string());
        }
        let km = members_of(ki);
        let bm = members_of(bi);
        for (mname, krec) in &km {
            match bm.get(mname) {
                Some(brec) => {
                    covered_members += 1;
                    let k_native = krec["native"].as_bool().unwrap_or(true);
                    let b_native = brec["native"].as_bool().unwrap_or(true);
                    let kind = krec["kind"].as_str().unwrap_or("");
                    if !k_native && b_native && (kind == "accessor" || kind == "method") {
                        lies.push(format!("{name}.{mname}"));
                    }
                    if krec["ownTell"].as_bool().unwrap_or(false) {
                        own_tells.push(format!("{name}.{mname}"));
                    }
                    if krec["kind"] != brec["kind"] || krec["enumerable"] != brec["enumerable"]
                        || krec["configurable"] != brec["configurable"]
                        || (kind == "accessor" && (krec["get"] != brec["get"] || krec["set"] != brec["set"])) {
                        shape_mismatch.push(format!("{name}.{mname} (korvus {} / brave {})",
                            krec["kind"].as_str().unwrap_or("?"), brec["kind"].as_str().unwrap_or("?")));
                    }
                    if k_native {
                        if kind == "method" {
                            if krec["fnName"].as_str().unwrap_or("") == "" {
                                name_defect.push(format!("{name}.{mname} (fn.name=\"\")"));
                            }
                            let (kl, bl) = (krec["length"].as_i64().unwrap_or(-1), brec["length"].as_i64().unwrap_or(-1));
                            if bl >= 0 && kl != bl {
                                length_defect.push(format!("{name}.{mname} (len={kl}, want {bl})"));
                            }
                        } else if kind == "accessor" {
                            if krec["get"].as_bool().unwrap_or(false) {
                                if krec["getName"].as_str().unwrap_or("") == "" {
                                    name_defect.push(format!("{name}.{mname} (getter name=\"\")"));
                                }
                                if krec["getLength"].as_i64().unwrap_or(0) != 0 {
                                    length_defect.push(format!("{name}.{mname} (getter len={})", krec["getLength"].as_i64().unwrap_or(0)));
                                }
                            }
                            if krec["set"].as_bool().unwrap_or(false) {
                                if krec["setName"].as_str().unwrap_or("") == "" {
                                    name_defect.push(format!("{name}.{mname} (setter name=\"\")"));
                                }
                                if krec["setLength"].as_i64().unwrap_or(1) != 1 {
                                    length_defect.push(format!("{name}.{mname} (setter len={})", krec["setLength"].as_i64().unwrap_or(1)));
                                }
                            }
                        }
                    }
                }
                None => extra_members.push(format!("{name}.{mname}")),
            }
        }
    }

    // Global-scope diff: korvus-only globals are tells; a real worker exposes none of them.
    let empty: Vec<serde_json::Value> = vec![];
    let k_glob: Vec<&str> = korvus["globals"].as_array().unwrap_or(&empty).iter().filter_map(|v| v.as_str()).collect();
    // The baseline globals BOTH ways: as a set, for the two membership columns below, and in the order
    // the capture recorded them, for the ORDER column -- `Object.getOwnPropertyNames` is a
    // sequence a page can read, and a set comparison is blind to every permutation of it.
    let b_glob_ordered: Vec<&str> = brave["globals"].as_array().unwrap_or(&empty).iter().filter_map(|v| v.as_str()).collect();
    let b_glob: BTreeSet<&str> = b_glob_ordered.iter().copied().collect();
    let k_glob_set: BTreeSet<&str> = k_glob.iter().cloned().collect();
    let order = webapi_gen::order::compare(&b_glob_ordered, &k_glob);
    let global_tells: Vec<String> = k_glob.iter().filter(|g| !b_glob.contains(*g)).map(|s| s.to_string()).collect();
    let missing_globals: Vec<String> = b_glob.iter().filter(|g| !k_glob_set.contains(*g)).map(|s| s.to_string()).collect();

    // ---- SINGLETONS: the section this scoreboard was loading and never comparing ----
    //
    // `navigator` is not a constructor, so it is not in `interfaces`, and it is not a constructor NAME,
    // so it is not in `globals` either. It lives in `singletons`, which the baseline has carried since
    // it was captured -- with all 23 of its members -- and which nothing here looked at. That is how
    // `typeof navigator === "undefined"` inside a worker survived a scoreboard that was holding the
    // answer: the board was thorough about the sections it compared and silent about the one it did
    // not. A zero that means "I did not look" is worse than a number that means "this is broken".
    let no_singles = serde_json::Map::new();
    let k_singles = korvus["singletons"].as_object().unwrap_or(&no_singles);
    let b_singles = brave["singletons"].as_object().unwrap_or(&no_singles);
    let missing_singletons: Vec<String> =
        b_singles.keys().filter(|k| !k_singles.contains_key(*k)).cloned().collect();
    let extra_singletons: Vec<String> =
        k_singles.keys().filter(|k| !b_singles.contains_key(*k)).cloned().collect();
    let mut missing_singleton_members: Vec<String> = Vec::new();
    let mut singleton_ctor_defect: Vec<String> = Vec::new();
    for (name, bv) in b_singles {
        // A singleton that is absent entirely is counted once above, not once per member -- otherwise
        // one missing object drowns the list it shares with genuinely partial ones.
        let Some(kv) = k_singles.get(name) else { continue };
        for mname in members_of(bv).keys() {
            if !members_of(kv).contains_key(mname) {
                missing_singleton_members.push(format!("{name}.{mname}"));
            }
        }
        let (bc, kc) = (bv["ctor"].as_str(), kv["ctor"].as_str());
        if bc != kc {
            singleton_ctor_defect.push(format!("{name}: brave {bc:?} vs korvus {kc:?}"));
        }
    }

    let pct = |a: usize, b: usize| if b == 0 { 0.0 } else { 100.0 * a as f64 / b as f64 };
    println!("\n========== WORKER surface parity: Korvus vs Brave 1.93.136 / Chromium 151 (DedicatedWorkerGlobalScope) ==========");
    println!("INTERFACES  korvus {:>4} / brave {:>4}   coverage {:>5.1}%   (missing {}, korvus-only {})",
        k_ifaces.len(), b_ifaces.len(), pct(present.len(), b_ifaces.len()), missing.len(), extra_ifaces.len());
    println!("MEMBERS     korvus {:>4} / brave {:>4}   coverage {:>5.1}%   (covered-on-shared {})",
        k_member_total, b_member_total, pct(covered_members, b_member_total), covered_members);
    println!("LIES        {:>4}   (korvus members NOT [native code] where brave IS native)  <- DoD debt", lies.len());
    println!("TELLS       {:>4}   (korvus-only members on shared interfaces; e.g. leaked _helpers)", extra_members.len());
    println!("SHAPE DIFF  {:>4}   (kind/enumerable mismatches on shared members)", shape_mismatch.len());
    println!("NAME DEFECT {:>4}   (native fn.name != canonical; e.g. raw v8 fn reports \"\")", name_defect.len());
    println!("LEN DEFECT  {:>4}   (native fn.length wrong vs brave / 0-getter / 1-setter)", length_defect.len());
    println!("CTOR-NAME   {:>4}   (interface ctor.name != WebIDL name)", ctor_name_defect.len());
    println!("OWN-TELLS   {:>4}   (native member fn owns prototype/arguments/caller -- JS-shim tell)", own_tells.len());
    println!("PROTO DIFF  {:>4}   (prototype chain != brave)", proto_mismatch.len());
    println!("TAG DIFF    {:>4}   (Symbol.toStringTag != brave)", tag_mismatch.len());
    println!("GLOBAL-TELLS{:>4}   (korvus-only globals: __* bridges, Deno, print, ...; real workers have none)", global_tells.len());
    println!("MISSING-GLOB{:>4}   (baseline globals korvus's worker lacks)", missing_globals.len());
    println!("GLOB-ORDER  {:>4}   (own-globals in the wrong relative order: common {} - LCRO {})",
        order.out_of_place, order.common_len, order.lcro);
    println!("MISSING-SING{:>4}   (baseline SINGLETON OBJECTS korvus's worker lacks -- navigator, screen, ...)", missing_singletons.len());
    println!("SING-MEMBERS{:>4}   (members missing from singletons korvus DOES have)", missing_singleton_members.len());
    println!("SING-CTOR   {:>4}   (singleton constructor.name != brave -- e.g. Navigator vs WorkerNavigator)", singleton_ctor_defect.len());
    println!("SING-EXTRA  {:>4}   (korvus-only singletons; a worker has no window/document)", extra_singletons.len());

    let sample = |label: &str, v: &[String], n: usize| {
        let mut s: Vec<String> = v.to_vec(); s.sort();
        println!("\n-- {label} (showing {}/{}) --", n.min(s.len()), s.len());
        for x in s.iter().take(n) { println!("   {x}"); }
    };
    let mut miss_sorted: Vec<&&String> = missing.iter().collect(); miss_sorted.sort();
    println!("\n-- MISSING interfaces (showing {}/{}) --", 60.min(missing.len()), missing.len());
    for x in miss_sorted.iter().take(60) { println!("   {x}"); }
    let mut extra_if_sorted: Vec<&&String> = extra_ifaces.iter().collect(); extra_if_sorted.sort();
    println!("\n-- KORVUS-ONLY interfaces (tells; showing {}/{}) --", 40.min(extra_ifaces.len()), extra_ifaces.len());
    for x in extra_if_sorted.iter().take(40) { println!("   {x}"); }
    sample("TELLS: korvus-only members (real workers don't expose them)", &extra_members, 40);
    sample("LIES: make these native", &lies, 30);
    sample("NAME DEFECTS: native fns missing set_name (raw v8 callbacks)", &name_defect, 30);
    sample("LEN DEFECTS: native fns with wrong length", &length_defect, 30);
    sample("SHAPE DIFF", &shape_mismatch, 30);
    sample("CTOR-NAME defects", &ctor_name_defect, 30);
    sample("GLOBAL-TELLS (korvus-only worker globals -- relocate/delete)", &global_tells, 60);
    sample("MISSING-GLOB (baseline worker globals korvus lacks)", &missing_globals, 60);
    sample("OWN-TELLS", &own_tells, 30);
    sample("PROTO DIFF", &proto_mismatch, 30);
    sample("TAG DIFF", &tag_mismatch, 30);
    sample("MISSING-SING (baseline singleton objects this worker has none of)", &missing_singletons, 20);
    sample("SING-MEMBERS (members missing from singletons that DO exist)", &missing_singleton_members, 40);
    sample("SING-CTOR defects", &singleton_ctor_defect, 20);
    sample("SING-EXTRA (korvus-only singletons)", &extra_singletons, 20);

    print!("{}", order.render("WORKER (DedicatedWorkerGlobalScope)", 40));
    // The whole sequence, on request: a count says how bad and the witness list says which names, but
    // only the full order beside the bundle's shows the SHAPE of the difference — which is how the fix
    // for this realm was sized (see the ratchet's note below). Mirrors `KORVUS_GLOBAL_ORDER_OUT` in
    // `korvus-runtime/tests/global_order.rs`, which does the same for the window.
    if let Ok(out) = std::env::var("KORVUS_GLOBAL_ORDER_OUT") {
        let out = format!("{out}.worker.{id}.json");
        std::fs::write(&out, serde_json::to_string_pretty(&k_glob).unwrap())
            .unwrap_or_else(|e| panic!("KORVUS_GLOBAL_ORDER_OUT write to {out} failed: {e}"));
        eprintln!("wrote the worker's own-global order -> {out}");
    }

    // Ratchet floor (stays green while the gap is closed; tighten toward diff=0 as W2 lands).
    assert!(present.len() >= 10, "worker interface overlap regressed: {}", present.len());

    // ── THE DIFF IS ZERO, AND IT STAYS ZERO (goal invariant I8) ──────────────────────
    //
    // Every column above printed 0 for the first time at the commit that made `WorkerGlobalScope`,
    // `DedicatedWorkerGlobalScope` and `WorkerLocation` `interface!` tables and deleted
    // `worker-finalize.js`. I8's own wording is "byte-identical to the bundle's worker surface
    // (ratchets at 0)", so they are ASSERTED rather than printed: a printed number nobody asserts is
    // how the last three of these sat at 1, 9 and 2 for a month.
    assert!(missing.is_empty(), "worker interfaces the baseline has and this realm lacks: {missing:?}");
    assert!(extra_ifaces.is_empty(), "korvus-only worker interfaces (a tell): {extra_ifaces:?}");
    assert_eq!(covered_members, b_member_total, "every baseline worker member must be present");
    assert!(lies.is_empty(), "worker members that are not [native code] where Brave's are: {lies:?}");
    assert!(extra_members.is_empty(), "korvus-only members on shared worker interfaces: {extra_members:?}");
    assert!(shape_mismatch.is_empty(), "worker member shape differs from the baseline: {shape_mismatch:?}");
    assert!(name_defect.is_empty(), "worker native fns with the wrong name: {name_defect:?}");
    assert!(length_defect.is_empty(), "worker native fns with the wrong length: {length_defect:?}");
    assert!(ctor_name_defect.is_empty(), "worker interface ctor.name defects: {ctor_name_defect:?}");
    assert!(own_tells.is_empty(), "worker native fns owning prototype/arguments/caller: {own_tells:?}");
    assert!(proto_mismatch.is_empty(), "worker prototype chains differ from the baseline: {proto_mismatch:?}");
    assert!(tag_mismatch.is_empty(), "worker Symbol.toStringTag differs from the baseline: {tag_mismatch:?}");
    assert!(global_tells.is_empty(), "korvus-only worker globals (a real worker has none): {global_tells:?}");
    assert!(missing_globals.is_empty(), "baseline worker globals this realm lacks: {missing_globals:?}");

    // SINGLETON RATCHET. These four numbers are the measured state on 2026-08-29, the first run that
    // ever compared this section. They are a CEILING, not a target: nothing may make the worker's
    // singleton layer worse, and every one of them must reach 0 when Track 4.4
    // lands. Tighten them as it does.
    //
    // Why they are not asserted at 0 today: they are all pre-existing, and turning them red now would
    // add a failure to the standing suite that every in-flight unit gates against, for a defect none of
    // those units introduced. The ratchet is how this codebase already handles that (see the pixel
    // ratchet in scripts/gate-plugin.sh and OPS_AT_MIGRATION in tests/registry.rs).
    //
    // What they mean, in one line each, because the numbers alone do not say it:
    //   navigator does not exist in a worker at all -- typeof navigator === "undefined".
    //   crypto, location and performance are PLAIN OBJECT LITERALS: their constructor.name is "Object"
    //   against Brave's Crypto / WorkerLocation / Performance, and their members are own properties on
    //   Object.prototype-rooted objects rather than accessors on a real prototype. worker-host.js:210
    //   is the clearest case -- `globalThis.location = { href, origin }`, two own properties where a
    //   WorkerLocation has ten on its prototype.
    //
    // ALL THREE ARE ZERO NOW, and the last of them closed: `WorkerLocation` is an
    // `interface!` table (core/korvus-webapi/src/core/workers/worker_global_scope.rs) whose ten rows
    // read this isolate's own script URL, minted in Rust and published for the
    // `WorkerGlobalScope.location` row -- so the object literal quoted above is gone and with it the
    // last 9 missing singleton members and the last "Object" constructor name. `performance` closed in
    // W4.d and `crypto` in W5.1 L15. Nothing may raise them again.
    const SINGLETONS_MISSING: usize = 0;      // navigator now exists (install_worker_navigator)
    // Ratcheted 25 -> 22 -> 0 (W5.1 L15's `Crypto` table, then W6.2's `WorkerLocation` table).
    const SINGLETON_MEMBERS_MISSING: usize = 0;
    // Ratcheted 3 -> 2 -> 0: `crypto`, then `location`; `performance` reported its real name from W4.d.
    const SINGLETON_CTOR_DEFECTS: usize = 0;
    assert!(missing_singletons.len() <= SINGLETONS_MISSING,
        "a baseline singleton went missing from the worker realm: {missing_singletons:?}");
    assert!(missing_singleton_members.len() <= SINGLETON_MEMBERS_MISSING,
        "worker singleton members regressed ({} > {SINGLETON_MEMBERS_MISSING}): {missing_singleton_members:?}",
        missing_singleton_members.len());
    assert!(singleton_ctor_defect.len() <= SINGLETON_CTOR_DEFECTS,
        "a worker singleton lost its constructor identity: {singleton_ctor_defect:?}");
    // Zero, and it stays zero: a korvus-only singleton in a worker (a `window` or a `document`) would be
    // a tell no real worker can produce, so this one has no debt to ratchet down.
    assert!(extra_singletons.is_empty(), "korvus-only worker singletons: {extra_singletons:?}");

    // ── OWN-GLOBAL ORDER ───────────────────────────────────────────────────────────
    //
    // `MISSING-GLOB 0` and `GLOBAL-TELLS 0` above say the worker exposes exactly the bundle's 335
    // names. They say nothing about the SEQUENCE, and a page reads the sequence: this realm agrees
    // with the bundle on 61 of 335 positions, diverging first at index 61 where the bundle has
    // `WebSocketStream` and this realm has `Temporal`. That gap was measured twice this session by
    // units that reverted their probes, so it was folklore until this line.
    //
    // WHAT IS RATCHETED, and why it is not the 274-positions-differ number those probes reported:
    // `positions` moves by the whole tail for a single relocated name, so it cannot distinguish one
    // misplaced global from a shuffled realm, and it goes DOWN when a correctly-placed name is
    // inserted. `out_of_place` (common - LCRO) is the minimum number of names that must be re-inserted
    // to make the two orders agree -- a defect count, one per misplaced name, and unchanged by a
    // correct insertion. `webapi_gen::order`'s unit tests pin exactly that behaviour on reorder /
    // insert / remove / relocate, so this number's meaning is itself under test.
    //
    // NON-VACUITY FIRST. The denominator is asserted before the ratchet, because a comparison over an
    // empty intersection scores a perfect 0 -- the `present.len() >= 10` failure mode this file already
    // shipped once (a 292/294 regression that passed).
    assert_eq!(order.common_len, b_glob_ordered.len(),
        "the order comparison lost its denominator: {} common names against a {}-name baseline",
        order.common_len, b_glob_ordered.len());
    // Measured 2026-09-12 at e93dabf7: 335 common names, LCRO 215, 120 out of place, and 61 of 335
    // positions agreeing. At `fd6473b8`, under each bundle: brave-151 120, chrome-150 119 (334 common),
    // edge-151 120, opera-135 120.
    //
    // ZERO, UNDER EVERY BUNDLE. A plan sized the fix as 120 -> 6 and called the 6
    // (`Temporal`, `SuppressedError`, `DisposableStack`, `AsyncDisposableStack`, `Float16Array`,
    // `WebAssembly`, which V8 creates before any install) irreducible by re-sequencing installs. The
    // registry walk's last step, `installer::seat_globals_in_capture_order`, moves them anyway: it
    // deletes and re-defines each own global from the first divergence on, with its own descriptor, in
    // the worn bundle's `globals` order, and V8 appends a re-defined global to the enumeration order.
    // The window realm's copy of this note, in `korvus-runtime/tests/global_order.rs`, has the rest.
    // A CEILING: it cannot fall further, and it may not rise.
    const WORKER_GLOBALS_OUT_OF_PLACE: usize = 0;
    assert!(order.out_of_place <= WORKER_GLOBALS_OUT_OF_PLACE,
        "[{id}] worker own-global ORDER regressed: {} names out of place (ceiling {WORKER_GLOBALS_OUT_OF_PLACE}). \
         The order a page reads got further from the bundle's.\n{}",
        order.out_of_place, order.render("WORKER (DedicatedWorkerGlobalScope)", 60));
}
