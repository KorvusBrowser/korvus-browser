//! The behavior-corpus DEV LOOP: what we answered vs what the browser recorded, case by case.
//!
//! NOT a gate -- it asserts nothing about quality, and `scripts/coverage.sh` remains the measurement.
//! This is the read the worklist needs and the gate does not print: `coverage.sh` reports a per-member
//! pass/total, so a member at 0/3 gives no clue WHICH answer is wrong or what we said instead.
//!
//! Same boot, same page, same runner and same damage-fingerprint reload as `tests/quality.rs`, so its
//! numbers ARE the gate's numbers -- the whole corpus is evaluated and only the PRINTING is narrowed,
//! because page state is order-dependent (one interface's destructive case forces the reload the next
//! interface is measured after) and a corpus subset would answer a different question.
//!
//!   KORVUS_SLICE=Document,Element cargo test -p korvus-webapi --test slice_probe -- --ignored --nocapture
//!
//! prints `FAIL <iface>::<member>` with the expression, our answer and the recorded one for the named
//! interfaces; a `COUNT <n> <iface>` line for EVERY interface (so a change's collateral is visible --
//! fixing one member routinely moves another interface through a shared helper); and a `PROBE` total.
//! `KORVUS_EVAL=<expr>` instead evaluates one expression in the booted engine and prints it, which is
//! how to ask the engine a question without writing a fixture.

use engine::Engine;
use webapi_gen::oracle;

const RUNNER: &str = include_str!("fixtures/behavior-run.js");
const PAGE: &str = include_str!("fixtures/snapshot-page.html");


#[ignore = "dev probe: boots V8"]
#[tokio::test]
async fn slice_probe() {
    let want: Vec<String> = std::env::var("KORVUS_SLICE")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    // The WHOLE corpus runs, exactly as the grader runs it — page state is order-dependent (one
    // interface's destructive case forces the reload the NEXT interface is measured after), so a
    // narrowed run answers a different question than the gate does. Only the printing is narrowed.
    let (cases, _) = oracle::from_dir(&persona::shipped_artifact("oracle"));
    assert!(!cases.is_empty(), "no behavior cases loaded");

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();

    // Same damage fingerprint as tests/quality.rs: reload ONLY when a case broke the page, so the
    // probe sees exactly the page state the grader sees.
    const FINGERPRINT: &str = r#"(() => { try {
        const d = document;
        return [d.childNodes.length,
                d.doctype ? d.doctype.name : "-",
                d.documentElement ? d.documentElement.tagName : "-",
                d.body ? d.body.childNodes.length : -1,
                d.head ? d.head.childNodes.length : -1].join("|");
      } catch (err) { return "err"; } })()"#;
    let fingerprint = |e: &mut Engine| -> String {
        e.eval_value("fp", FINGERPRINT.to_string())
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "err".into())
    };
    if let Ok(src) = std::env::var("KORVUS_EVAL") {
        println!("EVAL {:?}", e.eval_value("eval", src));
        return;
    }
    let pristine = fingerprint(&mut e);

    let mut start = 0usize;
    let mut results = serde_json::Map::new();
    while start < cases.len() {
        let iface = cases[start].iface.clone();
        let mut end = start;
        while end < cases.len() && cases[end].iface == iface {
            end += 1;
        }
        let pairs: Vec<serde_json::Value> = cases[start..end]
            .iter()
            .enumerate()
            .map(|(i, c)| serde_json::json!({ "id": (start + i).to_string(), "expr": c.expr }))
            .collect();
        let src = format!(
            "globalThis.__KORVUS_CASES = {};\n{RUNNER}",
            serde_json::to_string(&pairs).unwrap()
        );
        let raw = e.eval_value("probe", src).unwrap();
        let group: serde_json::Value =
            raw.as_str().and_then(|s| serde_json::from_str(s).ok()).expect("runner json");
        if let serde_json::Value::Object(m) = group {
            results.extend(m);
        }
        // A case that damaged the page is repaired before the next interface is measured, and the
        // repair is REPORTED: a corpus that damages the fixture is a fact about the corpus, and it
        // used to be one interface's destructive case silently reshaping every later answer.
        if fingerprint(&mut e) != pristine {
            println!("DAMAGE after {iface}");
            e.load_html(PAGE).unwrap();
            e.run().await.unwrap();
        }
        start = end;
    }

    let mut fails = 0;
    let mut total = 0;
    // Every interface's fail count, printed at the end: a change in one interface's implementation
    // routinely moves another's answers (shared helpers, and the page state a destructive case
    // leaves behind), and a slice-only report cannot see that.
    let mut per_iface: std::collections::BTreeMap<String, usize> = Default::default();
    for (i, c) in cases.iter().enumerate() {
        total += 1;
        let r = results.get(&i.to_string());
        let (ours, threw, undef) = match r {
            Some(r) if r.get("threw").and_then(|v| v.as_str()).is_some() => {
                (None, r["threw"].as_str().map(str::to_string), false)
            }
            Some(r) if r.get("undef").and_then(|v| v.as_bool()) == Some(true) => (None, None, true),
            Some(r) if r.get("value").is_some() => (Some(r["value"].clone()), None, false),
            _ => (None, None, false),
        };
        let mut obs = oracle::BehaviorObs::default();
        obs.observe_full(c, ours.as_ref(), threw.as_deref(), undef);
        if obs.pass == 0 {
            fails += 1;
            *per_iface.entry(c.iface.clone()).or_default() += 1;
            if !want.iter().any(|w| *w == c.iface) {
                continue;
            }
            let got = match (&ours, &threw, undef) {
                (_, Some(t), _) => format!("threw {t}"),
                (_, None, true) => "undefined".into(),
                (Some(v), None, false) => v.to_string(),
                (None, None, false) => "(unrepresentable)".into(),
            };
            let want = match (&c.throws, c.undefined, &c.expected) {
                (Some(t), _, _) => format!("throws {t}"),
                (None, true, _) => "undefined".into(),
                (None, false, Some(v)) => v.to_string(),
                (None, false, None) => "(not to throw)".into(),
            };
            println!("FAIL {}::{}\n  expr: {}\n  ours: {got}\n  want: {want}", c.iface, c.member, c.expr);
        }
    }
    for (iface, n) in &per_iface {
        println!("COUNT {n}\t{iface}");
    }
    println!("PROBE {} fail / {} total", fails, total);
}
