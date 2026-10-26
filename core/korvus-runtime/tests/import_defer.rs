//! korvus's JavaScript feature set is Chrome's, not Deno's — and the crash that hid behind the
//! difference.
//!
//! deno_core hard-codes `--js-defer-import-eval` and `--js-source-phase-imports` before
//! `V8::initialize()`. Chrome 150 implements neither: both forms are a parse error there, and a page
//! that asks gets a `SyntaxError`. korvus answered them, which is a page-readable divergence in the
//! page realm with no privileged API — and, for the defer half, a process abort:
//! `deno_core-0.410.0/modules/map.rs:2424`, `assert_eq!(module.get_status(), Evaluated)` with a
//! deliberately-unevaluated deferred module on the left. `src/v8_features.rs` has the mechanism and
//! the Chrome table; this file pins the answers.
//!
//! EVERY EXPECTATION BELOW WAS MEASURED IN CHROME 150.0.7871.46 on 2026-09-12, `--headless=new`
//! against a loopback `python3 -m http.server`, reading the probe page's own DOM (`--dump-dom`):
//!
//! ```text
//!   import.defer            => SyntaxError: Unexpected identifier 'defer'
//!   import source           => SyntaxError: Unexpected identifier 'source'
//!   using decl              => PARSES
//!   Symbol.dispose          => symbol
//!   DisposableStack         => function
//!   Float16Array            => function
//!   Temporal                => object
//! ```
//!
//! The last five are why this file also asserts what is still ON: the fix is "Chrome's feature set",
//! not "fewer features", and a future flag sweep that turned off `--js-explicit-resource-management`,
//! `--js-float16array` or `--harmony-temporal` would move korvus AWAY from Chrome. Those three go
//! red here if anyone does.
use engine::Engine;

/// `new Function` rather than a literal, because `import.defer` is a PARSE error: written literally
/// it would kill this file's own script before any assertion ran — which is exactly why the Chrome
/// probe was written the same way.
fn parses(src: &str) -> String {
    format!("(() => {{ try {{ new Function({src}); return 'PARSES'; }} catch (e) {{ return e.name; }} }})()")
}

async fn page_realm(expr: String) -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.eval_value("probe", expr).expect("eval")
}

/// THE CRASH, at its source. `import.defer` must not parse — and because it does not, deno_core's
/// unsound Defer path (instantiate, never evaluate, then assert Evaluated) is unreachable rather
/// than merely untaken.
#[tokio::test]
async fn import_defer_is_a_syntax_error_as_it_is_in_chrome() {
    let v = page_realm(parses(r#""return import.defer('./dep.mjs')""#)).await;
    assert_eq!(
        v.as_str(),
        Some("SyntaxError"),
        "Chrome 150 answers `SyntaxError: Unexpected identifier 'defer'`; korvus must not implement \
         a proposal Chrome does not ship — a page can read the difference, and deno_core's Defer \
         path aborts the process (map.rs:2424, Instantiated != Evaluated)"
    );
}

/// The other import-phase proposal deno_core turns on. Same measurement, same verdict.
#[tokio::test]
async fn import_source_is_a_syntax_error_as_it_is_in_chrome() {
    let v = page_realm(parses(r#""return import.source('./dep.mjs')""#)).await;
    assert_eq!(
        v.as_str(),
        Some("SyntaxError"),
        "Chrome 150 answers `SyntaxError: Unexpected identifier 'source'`"
    );
}

/// The guard on the other direction: the three deno_core flags that MATCH Chrome stay on. Without
/// this, "match Chrome" could be satisfied by turning deno_core's whole list off, which would be
/// four new divergences rather than none.
#[tokio::test]
async fn the_features_chrome_does_ship_are_still_here() {
    let v = page_realm(
        "JSON.stringify({ \
           using: (() => { try { new Function('{ using x = { [Symbol.dispose]() {} }; }'); return 'PARSES'; } catch (e) { return e.name; } })(), \
           dispose: typeof Symbol.dispose, \
           stack: typeof DisposableStack, \
           f16: typeof Float16Array, \
           temporal: typeof globalThis.Temporal })"
            .to_string(),
    )
    .await;
    let got: serde_json::Value = serde_json::from_str(v.as_str().expect("json string")).expect("parse");
    assert_eq!(got["using"], "PARSES", "Chrome 150 parses a `using` declaration");
    assert_eq!(got["dispose"], "symbol", "Chrome 150 has Symbol.dispose");
    assert_eq!(got["stack"], "function", "Chrome 150 has DisposableStack");
    assert_eq!(got["f16"], "function", "Chrome 150 has Float16Array");
    assert_eq!(got["temporal"], "object", "Chrome 150 has Temporal");
}

/// THE WPT FILE THAT KILLED THE TIER, in one process, as a page.
///
/// `tla-deferred-evaluation.tentative.html` is `import.defer` of a module whose dependency is a
/// top-level await — the shape that leaves a `DynImportModEvaluate` pending, so
/// `evaluate_dyn_imports` hands an Instantiated module to `dynamic_import_resolve`. Before the fix
/// this aborted the process and `shard 3/11` of `scripts/wpt-tiers.sh core` never wrote its JSON.
/// The assertion is deliberately about SURVIVING with a verdict, not about the proposal's semantics:
/// Chrome does not implement it either, and a page that uses it gets a syntax error from both.
#[tokio::test]
async fn the_deferred_import_page_does_not_abort_the_process() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(
        r#"<!doctype html><html><body><script type="module">
           globalThis.__reached = 'module-ran';
           const ns = await import.defer("./resources/module-with-tla-dep.js");
           globalThis.__reached = 'defer-resolved';
           </script></body></html>"#,
        "http://127.0.0.1:8731/tla.html",
        &[],
    )
    .await
    .expect("load_page");
    e.run().await.expect("run");
    // The module does not compile at all — `import.defer` is a parse error — so the body never runs
    // and the slot stays unset. What matters is that the process is still here to be asked.
    let v = e.eval_value("probe", "String(globalThis.__reached)".to_string()).expect("eval");
    assert_eq!(
        v.as_str(),
        Some("undefined"),
        "an inline module containing `import.defer` must fail to COMPILE, as it does in Chrome, so \
         not even its first statement runs"
    );
}

/// The same answer inside a Worker. The V8 flags are process-global, so this cannot fail on its own
/// — but the worker realm is a separate isolate on a separate thread, `worker.rs` creates its
/// `JsRuntime` through the same choke point, and this engine has been bitten before by a surface that was
/// only ever checked in the page realm. Driven the way `dom_point_worker_live.rs` drives one.
#[tokio::test]
async fn a_worker_answers_the_same() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_document("<!doctype html><html><head></head><body></body></html>").expect("document");
    let body = "try { new Function(\"return import.defer('./d.mjs')\"); self.postMessage('PARSES'); } \
                catch (e) { self.postMessage(e.name); }";
    e.eval(
        "worker-scripts",
        format!("globalThis.__workerScripts = {{ './probe.js': {} }};", serde_json::to_string(body).unwrap()),
    )
    .expect("register");
    e.run().await.expect("run");
    e.eval("driver", "const w = new Worker('./probe.js'); w.onmessage = (ev) => print('RESULT:' + ev.data);".into())
        .expect("spawn");
    let start = std::time::Instant::now();
    let mut out = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("RESULT:")) || start.elapsed() > std::time::Duration::from_secs(20) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let got = out
        .iter()
        .find(|l| l.starts_with("RESULT:"))
        .map(|l| l["RESULT:".len()..].to_string())
        .unwrap_or_else(|| format!("NONE (output: {out:?})"));
    assert_eq!(got, "SyntaxError", "a worker realm must answer what the page realm answers");
}
