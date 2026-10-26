//! The behavior runner's own contract, checked inside Korvus.
//!
//! `tests/fixtures/behavior-run.js` is evaluated by four callers, and two of them --
//! `tests/quality.rs` here and `plugins/korvus-persona/capture/collect.js` inside the real browser -- have
//! their answers compared to each other case by case. That comparison is only meaningful if both
//! evaluate under the SAME rules. A difference in how a value is narrowed, how a thrown error is
//! named, or (new, and the dangerous one) whether a promise is settled at all would show up in the
//! corpus as a behavior difference that is really a harness difference, and nothing downstream could
//! tell the two apart.
//!
//! So both sides run the same fixture and must produce the same map:
//!
//! ```text
//! cargo test -p korvus-webapi --test behavior_runner_async      # this file: Korvus's side
//! python3 plugins/korvus-persona/capture/host.py   # then open it in the browser: a real browser's side
//! ```
//!
//! `tests/fixtures/behavior-run-selftest.json` is NOT part of the behavior corpus and no entry in it
//! asks a question about a WebAPI member: every expected outcome is a property of the runner. That is
//! why the outcomes may be written by hand here while a corpus `expected` may never be.


use engine::Engine;

const RUNNER: &str = include_str!("fixtures/behavior-run.js");
const SELFTEST: &str = include_str!("fixtures/behavior-run-selftest.json");
/// The same page the quality harness loads, so `document` and DOM globals are real.
const PAGE: &str = include_str!("fixtures/snapshot-page.html");

/// Above the runner's own 1500 ms per-case deadline plus room for the fixture's slow-but-settling
/// case; a runner that hangs must fail this test rather than the suite's global timeout.
const BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

fn probes() -> Vec<serde_json::Value> {
    let all: Vec<serde_json::Value> = serde_json::from_str(SELFTEST).expect("selftest fixture parses");
    // The first entry is the file's own explanation and carries no `id`; skip anything shaped like it
    // rather than keying on position, so a second note can be added without breaking this.
    all.into_iter().filter(|x| x.get("id").is_some()).collect()
}

#[tokio::test]
async fn the_runner_answers_awaited_and_synchronous_cases_by_the_documented_contract() {
    let probes = probes();
    assert!(probes.len() >= 20, "selftest fixture shrank unexpectedly: {}", probes.len());

    let pairs: Vec<serde_json::Value> = probes
        .iter()
        .map(|p| {
            let mut v = serde_json::json!({ "id": p["id"], "expr": p["expr"] });
            if p.get("await") == Some(&serde_json::Value::Bool(true)) {
                v["await"] = serde_json::Value::Bool(true);
            }
            v
        })
        .collect();

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();

    let src = format!(
        "globalThis.__KORVUS_CASES = {};\n{RUNNER}",
        serde_json::to_string(&pairs).unwrap()
    );
    let raw = e
        .eval_value_settled("behavior-run-selftest", src, BUDGET)
        .await
        .expect("the runner must settle");
    let got: serde_json::Value = raw
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .expect("the runner must return a JSON string (or a promise of one)");

    let mut bad = Vec::new();
    for p in &probes {
        let id = p["id"].as_str().unwrap();
        let want = &p["outcome"];
        let have = got.get(id).unwrap_or(&serde_json::Value::Null);
        if have != want {
            bad.push(format!("  {id}\n    want {want}\n    got  {have}\n    expr {}", p["expr"]));
        }
    }
    assert!(bad.is_empty(), "the runner broke its own contract:\n{}", bad.join("\n"));
}

/// A corpus with NO awaited case must still be answered SYNCHRONOUSLY -- a string, not a promise.
///
/// This is the property that lets ~10,000 recorded cases be trusted across the change. It is not
/// merely that they still get the same answer: they never reach the awaited path at all, so no
/// microtask or timer of the page's can run between two of them where none could before.
/// `Engine::eval_value` does not settle promises, so if the runner had gone async unconditionally
/// this would come back as `{}` (JSON.stringify of a Promise) instead of a string.
#[tokio::test]
async fn a_corpus_with_no_awaited_case_is_answered_without_touching_the_event_loop() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();

    let pairs = serde_json::json!([
        { "id": "a", "expr": "1 + 1" },
        { "id": "b", "expr": "Promise.resolve(1)" },
    ]);
    let raw = e
        .eval_value("behavior-run-sync-shape", format!("globalThis.__KORVUS_CASES = {pairs};\n{RUNNER}"))
        .expect("eval");
    let s = raw.as_str().expect("a synchronous corpus must return a STRING, not a promise");
    let m: serde_json::Value = serde_json::from_str(s).unwrap();
    assert_eq!(m["a"], serde_json::json!({ "value": 2 }));
    assert_eq!(
        m["b"],
        serde_json::json!({ "unsupported": true }),
        "an unflagged promise must stay unrecordable -- a global flip to await would silently \
         re-answer every member currently recorded as \"[object Promise]\""
    );
}

/// The corpus on disk must never carry an `await` flag the recorder did not put there, and every
/// awaited case must have been RECORDED (it has a `source`), not authored.
///
/// An awaited case whose `expected` was hand-written is the exact forgery the whole oracle exists to
/// prevent, and it is easier to commit by accident here than anywhere else: the settled value of a
/// promise is the kind of thing an implementer feels confident about.
#[tokio::test]
async fn every_awaited_case_in_the_corpus_carries_recorded_provenance() {
    let dir = persona::shipped_artifact("oracle");
    let (cases, bad) = webapi_gen::oracle::from_dir(&dir);
    assert!(bad.is_empty(), "unparseable behavior-case files: {bad:?}");
    let unrecorded: Vec<String> = cases
        .iter()
        .filter(|c| c.awaited && !c.is_recorded())
        .map(|c| format!("{}.{} :: {}", c.iface, c.member, c.expr))
        .collect();
    assert!(
        unrecorded.is_empty(),
        "awaited cases with no recorded source (an awaited answer must come from a capture):\n{}",
        unrecorded.join("\n")
    );
}
