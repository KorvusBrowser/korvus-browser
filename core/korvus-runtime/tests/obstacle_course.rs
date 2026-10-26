//! Capability obstacle-course gate — the Phase-C loop's objective function.
//! See docs/archive/goal-plugin-core/BENCHMARK-KPI.md. Each stage loads a self-owned fixture, lets its
//! scripts run, and reads the deterministic `window.__obstacle` marker. Self-owned fixtures
//! (the reference benchmark = reference only); the `window.__obstacle` marker contract is kept.
use engine::Engine;

// M0 stages are spec-fixed (self-assert), so persona is irrelevant — use the proven stub.
// (M1 switches to the captured Brave persona when the first browser-dependent, oracle-backed
// stage lands; that is when persona-pinning matters. Spec §2 hybrid rule.)
/// Every fixture sets `window.__obstacle` to a deterministic string; the check reads it back.
const CHECK: &str = "String(window.__obstacle || '')";

struct Stage {
    id: &'static str,
    html: &'static str,
    check: &'static str,
    expected: &'static str,
    gap: bool, // true = a known capability gap (red today), XFAIL-tracked
}

const STAGES: &[Stage] = &[
    Stage {
        id: "static",
        html: include_str!("fixtures/obstacle/static.html"),
        check: CHECK,
        expected: "static:ok",
        gap: false,
    },
    Stage {
        id: "es-modules",
        html: include_str!("fixtures/obstacle/es-modules.html"),
        check: CHECK,
        expected: "esm:ran:42:8:true",
        gap: false,
    },
    Stage {
        id: "mutation-observer",
        html: include_str!("fixtures/obstacle/mutation-observer.html"),
        check: CHECK,
        expected: "mo:1:3",
        gap: false,
    },
    Stage {
        id: "textdecoder-shiftjis",
        html: include_str!("fixtures/obstacle/textdecoder-shiftjis.html"),
        check: CHECK,
        expected: "sjis:こんにちは:shift_jis",
        gap: false,
    },
    Stage { id: "dialog", html: include_str!("fixtures/obstacle/dialog.html"), check: CHECK, expected: "dialog:true:false:confirmed", gap: false },
    Stage { id: "input-step", html: include_str!("fixtures/obstacle/input-step.html"), check: CHECK, expected: "step:10:25:25", gap: false },
    Stage { id: "custom-element", html: include_str!("fixtures/obstacle/custom-element.html"), check: CHECK, expected: "ce:2:ready:true:upgrade=true,init,ready", gap: false },
    Stage { id: "wasm-streaming", html: include_str!("fixtures/obstacle/wasm-streaming.html"), check: CHECK, expected: "wasm:true:true:true", gap: false },
    Stage { id: "document-fragment", html: include_str!("fixtures/obstacle/document-fragment.html"), check: CHECK, expected: "createDF.nt=11|template.content.nt=11|clone.nt=11,kids=2|appended=<span id=\"a\">A</span><b>x</b>y|before.kids=5,fragEmptied=true", gap: false },
];

#[tokio::test]
async fn obstacle_course() {
    let mut pass = 0usize;
    let mut gaps = 0usize;
    // (id, gap, ok, actual, expected) — `expected` is carried alongside so the second (assert) pass
    // doesn't need to re-index into STAGES.
    let mut rows: Vec<(&str, bool, bool, String, &str)> = Vec::new();

    // Pass 1: run every stage and collect results. No assertions here — the scoreboard and JSON
    // dump below must always emit, even when a stage is red, so a run mid-M1 development still
    // reports the full picture instead of panicking out before the scoreboard prints.
    for st in STAGES {
        // Fresh engine per stage — no cross-stage global leakage.
        let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
        e.run().await.unwrap(); // boot (Korvus bootstrap is well-formed)
        let _ = e.load_html(st.html); // fixture scripts may throw — a throw is a red, tolerate it
        // Bounded drain: returns instantly when the page settles, but caps a fixture with perpetual
        // timers (rAF/setInterval — coming with A.2 WPT / framework fixtures) so it can't hang the gate.
        let _ = e.pump(std::time::Duration::from_millis(2000)).await;
        let actual = e
            .eval_value("obstacle-chk", st.check.to_string())
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let ok = actual == st.expected;
        if st.gap {
            gaps += 1;
        }
        if !st.gap && ok {
            pass += 1;
        }
        rows.push((st.id, st.gap, ok, actual.clone(), st.expected));
    }

    // Scoreboard (stderr; mirrors surface_parity's println scoreboard).
    eprintln!("obstacle-course: {} pass · {} known-gap (of {} stages)", pass, gaps, STAGES.len());
    for (id, gap, ok, actual, _expected) in &rows {
        eprintln!("  [{}] {:<14} ok={} actual={:?}", if *gap { "GAP" } else { "   " }, id, ok, actual);
    }

    // Scoreboard skeleton: emit JSON when asked (mirrors surface_parity's $KORVUS_SURFACE_OUT).
    if let Ok(path) = std::env::var("KORVUS_OBSTACLE_OUT") {
        let stages: Vec<serde_json::Value> = rows
            .iter()
            .map(|(id, gap, ok, actual, _expected)| serde_json::json!({ "id": id, "gap": gap, "pass": ok, "actual": actual }))
            .collect();
        let out = serde_json::json!({ "stages": stages, "pass": pass, "known_gap": gaps });
        std::fs::write(&path, serde_json::to_string_pretty(&out).unwrap())
            .unwrap_or_else(|e| panic!("KORVUS_OBSTACLE_OUT write to {path} failed: {e}"));
    }

    // Pass 2: assert, now that the scoreboard/JSON are on record. Known-gap stages are XFAIL — they
    // MUST still fail (closing one requires a deliberate `gap: false` flip in STAGES, in the same
    // commit that fixes the underlying capability); every other stage MUST pass.
    for (id, gap, ok, actual, expected) in &rows {
        if *gap {
            assert!(
                !ok,
                "GAP CLOSED: stage `{}` now matches expected (`{}`) — flip `gap: false` in STAGES and commit the win",
                id, expected
            );
        } else {
            assert!(ok, "stage `{}` regressed: expected `{}`, got `{:?}`", id, expected, actual);
        }
    }
}
