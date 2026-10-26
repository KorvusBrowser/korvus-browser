//! The WINDOW-vs-WORKER asymmetry gate.
//!
//! `tests/worker_surface.rs` compares the worker's MEMBERS against the captured worker surface. It
//! cannot see an interface OBJECT whose `Function.prototype.toString` reports JS source in one realm
//! and `[native code]` in the other, because it compares members rather than interface objects — that
//! is how `Function.prototype.toString.call(EventTarget)` sat handing back a class body inside a
//! worker while every member of `EventTarget.prototype` graded clean.
//!
//! So this test runs ONE probe file (`fixtures/worker-scope-probe.js`) in BOTH of this engine's
//! realms and diffs the two dumps, and asserts that the differences are EXACTLY the ones Chrome has.
//! The reference set was read off `google-chrome --headless=new` 150.0.7871.46 running the same file
//! unmodified in a page and in a real (classic, same-origin, http-served) worker — `file://` cannot
//! be used, because a `file://` page may not start a Blob or same-directory worker at all.
//!
//! `KORVUS_SCOPE_OUT=<dir>` writes `window.json` / `worker.json` there for eyeballing.
use engine::Engine;
use std::collections::BTreeSet;

const PROBE: &str = include_str!("fixtures/worker-scope-probe.js");

/// The thirteen facts that differ between a Chrome page and a Chrome dedicated worker, in this probe's
/// vocabulary. Measured, not reasoned: see the module header for the command.
///
/// Each is a REAL platform difference, not an engine defect — a worker has a `WorkerLocation` rather
/// than a `Location`, a `WorkerNavigator` rather than a `Navigator`, no `console.memory`, an
/// `importScripts`, `self` on the scope prototype instead of the global, a two-argument
/// `webkitRequestFileSystem`, a `WorkerGlobalScope` in its TypeError messages, and a `Performance`
/// without the window-only Navigation Timing partials.
const CHROME_ASYMMETRIES: &[&str] = &[
    "behaviour importScripts",
    "behaviour protoSelf",
    // `generated_order::WINQ`'s arity pass ran in the WORKER realm too, over `globalThis`
    // with the interface name hard-coded to "Window", and `queueMicrotask` was the one worker global
    // it found unguarded -- so a worker's `queueMicrotask()` answered "Failed to execute
    // 'queueMicrotask' on 'Window'". Chrome says 'WorkerGlobalScope' (measured over a Blob worker,
    // `--headless=new`, 2026-09-12), which is what `worker_scope.rs`'s own `require_args` says once
    // the second table is out of the way. It is an asymmetry because the WINDOW realm still says
    // 'Window'.
    "behaviour queueMicrotaskNoArgs",
    "behaviour selfOwn",
    "behaviour setTimeoutNoArgs",
    "global webkitRequestFileSystem",
    "singleton console.own",
    "singleton location.ctor",
    "singleton location.own",
    "singleton location.proto",
    "singleton navigator.ctor",
    "singleton navigator.proto",
    "singleton performance.proto",
];
/// The window/worker asymmetries this engine has that Chrome does not — a CEILING: it may only
/// shrink, and the test FAILS if an entry here is no longer true, so a fix cannot leave a stale pin
/// behind. That is what happened: this list named four, and all four are gone.
///
/// W6.2 recorded the recipe (assign the two native classes in `worker-host.js`, call the reconcile
/// from `worker.rs` parent-before-child, and select the worker's member list) but could not land it,
/// because `reconcile_dom_matrix_family` did not exist on its base — that function is W5.1 L21's, and
/// L21 in turn could not touch the worker because it was W6.2's seam. The two halves met at merge and
/// the four entries went with them:
///
///   * `global DOMMatrix` / `global DOMMatrixReadOnly` — the worker had neither, because making them
///     hand tables removed them from the ONE registry both realms draw from (W6.1). Now present as
///     the native cppgc classes.
///   * `ifaceObject DOMMatrix.len` / `...ReadOnly.len` — the WINDOW reported ctor `length` 1 where
///     BOTH captures record 0, so the window disagreed with its own capture. L21's tables answer 0.
///
/// One measured step was not in W6.2's recipe and is worth keeping here: a member SELECTOR stops the
/// installer mounting `toString`/`setMatrixValue` in the worker, but the deno_core class TEMPLATE
/// realises them anyway, so the worker install must also DELETE them from the prototype. Verification
/// does not remove what it did not mount — which is exactly the obstacle L20 named when it stopped.
/// Measured after the selector alone landed: `worker_surface` read MEMBERS 3129/3127, TELLS 2, naming
/// those two.
const PRE_EXISTING: &[&str] = &[];

/// Chrome asymmetries this engine cannot reproduce yet. **EMPTY now**, and the entry
/// that was here is worth keeping in the history: `global webkitRequestFileSystem`.
///
/// Chrome's page has `webkitRequestFileSystem` with `length` 3 and its worker has it with `length` 2,
/// so the pair is an asymmetry there. This engine's WORKER had it (a `DedicatedWorkerGlobalScope` row,
/// length 2) and its WINDOW had no such global at all -- so the name was not in both realms and there
/// was nothing to differ. That missing window global was named here as the Window-globals lane's, and
/// FP5 is that lane: `core/html/window_extras.rs` installs it with `length` 3, which is the asymmetry
/// Chrome has, so this list is empty.
///
/// A CEILING, like [`PRE_EXISTING`]: it may only shrink.
const NOT_YET: &[&str] = &[];

async fn dump_window() -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    e.eval("worker_scope_probe", format!("print('WSCOPE:' + ({PROBE}));")).unwrap();
    collect(&mut e, "WSCOPE:", "window").await
}

async fn dump_worker() -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    let worker_src = format!("self.postMessage({PROBE});");
    let reg = format!(
        "globalThis.__workerScripts = {{ './scope.js': {} }};",
        serde_json::to_string(&worker_src).unwrap()
    );
    e.eval("worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval(
        "scope-driver",
        "const w = new Worker('./scope.js'); w.onmessage = (ev) => print('WSCOPE:' + ev.data);".into(),
    )
    .unwrap();
    collect(&mut e, "WSCOPE:", "worker").await
}

async fn collect(e: &mut Engine, tag: &str, name: &str) -> serde_json::Value {
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(line) = out.iter().find(|l| l.starts_with(tag)) {
            let s = line[tag.len()..].to_string();
            if let Ok(d) = std::env::var("KORVUS_SCOPE_OUT") {
                let _ = std::fs::create_dir_all(&d);
                let pretty = serde_json::from_str::<serde_json::Value>(&s)
                    .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| s.clone()))
                    .unwrap_or_else(|_| s.clone());
                std::fs::write(format!("{d}/{name}.json"), pretty).expect("write the scope dump");
            }
            return serde_json::from_str(&s).unwrap_or_else(|err| panic!("the {name} probe's JSON parses: {err}"));
        }
        if start.elapsed() > std::time::Duration::from_secs(30) {
            panic!("the {name} scope probe never reported within 30s; output: {out:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

fn names(v: &serde_json::Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().map(|x| x.as_str().unwrap_or("?").to_string()).collect()).unwrap_or_default()
}

/// The asymmetry KEYS (what differs), not the values — the values are the platform's and would pin
/// this test to one persona. A key is a fact that differs between the two realms for something BOTH
/// realms have; a name only one realm exposes is the exposure table, which `worker_surface.rs` grades.
fn asymmetries(win: &serde_json::Value, wk: &serde_json::Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let (wi, wo) = (&win["ifaceNat"], &wk["ifaceNat"]);
    if let (Some(a), Some(b)) = (wi.as_object(), wo.as_object()) {
        for (n, av) in a {
            let Some(bv) = b.get(n) else { continue };
            for k in ["nat", "tells", "name", "len"] {
                if av[k] != bv[k] {
                    out.insert(format!("ifaceObject {n}.{k}"));
                }
            }
        }
    }
    let gw: BTreeSet<String> = names(&win["globals"]).into_iter().collect();
    let gk: BTreeSet<String> = names(&wk["globals"]).into_iter().collect();
    for n in gw.intersection(&gk) {
        if win["gdesc"][n] != wk["gdesc"][n] {
            out.insert(format!("global {n}"));
        }
    }
    if let (Some(a), Some(b)) = (win["singles"].as_object(), wk["singles"].as_object()) {
        for (n, av) in a {
            let Some(bv) = b.get(n) else { continue };
            if av["ctor"] != bv["ctor"] {
                out.insert(format!("singleton {n}.ctor"));
            }
            if av["own"] != bv["own"] {
                out.insert(format!("singleton {n}.own"));
            }
            if names(&av["proto"]) != names(&bv["proto"]) {
                out.insert(format!("singleton {n}.proto"));
            }
        }
    }
    if let (Some(a), Some(b)) = (win["behaviour"].as_object(), wk["behaviour"].as_object()) {
        for (n, av) in a {
            let Some(bv) = b.get(n) else { continue };
            if av != bv {
                out.insert(format!("behaviour {n}"));
            }
        }
    }
    out
}

#[tokio::test]
async fn the_worker_realm_differs_from_the_window_exactly_where_chrome_does() {
    let win = dump_window().await;
    let wk = dump_worker().await;
    assert_eq!(win["realm"], serde_json::json!("window"), "the window dump is the window's");
    assert_eq!(wk["realm"], serde_json::json!("worker"), "the worker dump is the worker's");

    let got = asymmetries(&win, &wk);
    let want: BTreeSet<String> = CHROME_ASYMMETRIES.iter().map(|s| s.to_string()).collect();
    let allowed: BTreeSet<String> = want.iter().cloned().chain(PRE_EXISTING.iter().map(|s| s.to_string())).collect();
    let extra: Vec<&String> = got.difference(&allowed).collect();
    let missing: Vec<&String> = want.difference(&got).filter(|n| !NOT_YET.contains(&n.as_str())).collect();
    assert!(
        extra.is_empty(),
        "this engine's realms differ where Chrome's do NOT ({} extra): {extra:#?}",
        extra.len()
    );
    // The ceiling only goes down: a pinned defect that got fixed has to leave the list in the same
    // commit, or the next one is free to reintroduce it.
    let stale: Vec<&&str> = PRE_EXISTING.iter().filter(|n| !got.contains(**n)).collect();
    assert!(stale.is_empty(), "PRE_EXISTING names an asymmetry this engine no longer has -- delete it: {stale:?}");
    let done: Vec<&&str> = NOT_YET.iter().filter(|n| got.contains(**n)).collect();
    assert!(done.is_empty(), "NOT_YET names an asymmetry this engine now reproduces -- delete it: {done:?}");
    assert!(
        missing.is_empty(),
        "this engine's realms agree where Chrome's DIFFER ({} missing): {missing:#?}",
        missing.len()
    );
}

/// The worker global's own prototype chain, and `self` reached through it rather than off the global.
/// Chrome: `DedicatedWorkerGlobalScope -> WorkerGlobalScope -> EventTarget -> Object`.
#[tokio::test]
async fn the_worker_global_is_chained_onto_its_scope_prototypes() {
    let wk = dump_worker().await;
    assert_eq!(
        names(&wk["chain"]),
        vec!["DedicatedWorkerGlobalScope", "WorkerGlobalScope", "EventTarget", "Object"],
        "the worker global's prototype chain"
    );
    assert_eq!(wk["behaviour"]["selfOwn"], serde_json::json!("OK:false"), "`self` is NOT an own global in a worker");
    assert_eq!(wk["behaviour"]["protoSelf"], serde_json::json!("OK:WorkerGlobalScope"), "`self` lives on WorkerGlobalScope.prototype");
    assert_eq!(wk["behaviour"]["selfIsGlobal"], serde_json::json!("OK:true"), "`self === globalThis`");
}
