//! The unified webapi quality harness — the single home for engine-booting webapi tests.
//! It consolidates the scattered surface/parity/oracle probes:
//! it boots the engine ONCE with the real `CapturedPersona`, evals the shared surface dumper to
//! snapshot the LIVE JS surface, then grades every interface (snapshot ∩ @webref corpus) with the same
//! v8-free aggregator (`webapi_gen`) the `webapi quality` CLI uses, printing one 5-axis report.
//!
//! Lives here (not in korvus-runtime) via a permitted dev-dependency cycle: this crate dev-depends on
//! `engine` (korvus-runtime) which depends on `webapi`. See Cargo.toml.
//!
//! Heavy (boots V8, walks every interface), so it is `#[ignore]` like the WPT scoreboard. Two env knobs:
//!   - `KORVUS_SNAPSHOT_OUT=<path>`  also writes the raw snapshot JSON there (the `webapi quality` CLI
//!     shells this to obtain a snapshot — see gen `get_snapshot`).
//!   - `KORVUS_WPT_JSON=<path>`      folds a WPT dump in as axis ⑤ (else the wpt column is `-`).
//!
//!   KORVUS_SNAPSHOT_OUT=/tmp/snap.json cargo test -p korvus-webapi --test quality -- --ignored --nocapture
//!
//! It asserts only a FLOOR (the probe ran and the aggregator produced grades), never a quality verdict:
//! the report REPRODUCES the scattered tests' numbers for a human to verify before deleting them, and a
//! hard verdict assertion would also break the CLI that shells this for its snapshot.
use std::path::PathBuf;

use engine::Engine;
use webapi_gen::{merge, oracle, profile, quality, snapshot, wpt};

/// The shared dumper — runs verbatim in a real browser AND here, so the two dumps are directly
/// diffable. Now the canonical copy (the legacy korvus-runtime parity probes include it cross-crate).
const DUMPER: &str = include_str!("fixtures/surface-dump.js");
/// The behavior-case evaluator, shared verbatim with `plugins/korvus-persona/capture/collect.js`.
/// The helper that feeds it a chunk of cases is [`behavior_runner_from`]; a bare `behavior_runner`
/// was deleted as unreferenced, and this line pointed at it until a gate said so.
const RUNNER: &str = include_str!("fixtures/behavior-run.js");
/// A small representative document so `document` + DOM-element instances resolve to real values.
const PAGE: &str = include_str!("fixtures/snapshot-page.html");
/// The INSTANCE axis (Track 0.8). Every other axis on this board walks prototypes; this one reads live
/// platform objects, which is the only place a Proxy-minted member or an Array wearing a list
/// interface's name is visible. Gated on its own in `tests/instance_tells.rs`; reported here so
/// `scripts/coverage.sh` prints it beside the other five.
const INSTANCE_TELLS: &str = include_str!("fixtures/instance-tells.js");
/// The PLAUSIBILITY axis (axis ⑦). The six axes above all grade what a member LOOKS like -- its
/// shape, its nativeness, its brand. None asks whether the ANSWER is plausible, which is how
/// `MediaSource.isTypeSupported` returned `false` for every string on earth while reading VERIFIED.
/// This probe hands a capability site a SET of inputs and reports only when every answer is
/// identical. Gated on its own in `tests/constant_answers.rs`; reported here so `scripts/coverage.sh`
/// prints it beside the other six.
const CONSTANT_ANSWERS: &str = include_str!("fixtures/constant-answers.js");
/// The behavioral oracle corpus (Pillar B mechanism), as a DIRECTORY of per-interface files
/// (`fixtures/behavior-cases/<Iface>.json`). The COMMON tester ships only module-agnostic `_smoke`
/// cases that verify the eval → diff path end-to-end; the per-MODULE correctness cases
/// (`URL.protocol == "https:"`, …) are per-case content the Implement/Enhance agent authors for the
/// module it is building — they are not the common tester's job.
///
/// One file per interface is what lets N campaign workers run in parallel: two agents authoring cases
/// for different interfaces never write the same path, so their worktrees merge cleanly. It is read at
/// RUNTIME rather than `include_str!`-ed, so editing cases no longer forces a harness rebuild.

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// First `n` items, with a `+k more` marker when truncated (compact report lines).
fn head(items: &[String], n: usize) -> Vec<String> {
    if items.len() <= n {
        items.to_vec()
    } else {
        let mut v = items[..n].to_vec();
        v.push(format!("+{} more", items.len() - n));
        v
    }
}

/// Everything captured from one engine boot: the surface snapshot, the behavior-eval results, and the
/// async UA-CH high-entropy values (`getHighEntropyValues`).
struct Capture {
    snapshot: String,
    behavior: serde_json::Value,
    uach: Option<serde_json::Value>,
    instance_tells: Vec<String>,
    constant_answers: Vec<String>,
}

/// Boot the engine ONCE with the real captured persona, load a representative page, then (1) dump the
/// surface, (2) eval every behavioral oracle `expr`, and (3) AWAIT the async UA-CH high-entropy values.
async fn capture(cases: &[oracle::BehaviorCase]) -> Capture {
    // The REAL captured identity so every `liveValue` is the profile value (axis ④), not a stub.
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();

    let raw = e.eval_value("surface-dump", DUMPER.to_string()).unwrap();
    let snapshot = raw
        .as_str()
        .expect("surface-dump.js must return a JSON string (JSON.stringify of the snapshot)")
        .to_string();

    let behavior = run_behavior_cases(&mut e, cases).await;

    // The gate (snapshot-only) doesn't need the async UA-CH report; skip its event-loop pump.
    let uach = if std::env::var("KORVUS_SNAPSHOT_ONLY").is_ok() { None } else { capture_uach(&mut e).await };

    // LAST: the instance probe provisions a <style> element if the page has none, so it must not run
    // before the surface dump or the behavior cases read the page.
    let instance_tells = e
        .eval_value("instance-tells", INSTANCE_TELLS.to_string())
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default();

    // AFTER the instance probe: this one appends a measurement fixture to the body (and removes it
    // again), so nothing that reads the page may run behind it.
    // The probe reports {findings, graded, skipped}; this report line prints the findings, and the
    // gate in tests/constant_answers.rs is the one that asserts the roster was actually asked.
    let constant_answers = e
        .eval_value("constant-answers", CONSTANT_ANSWERS.to_string())
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.get("findings")?.clone()).ok())
        .unwrap_or_default();

    Capture { snapshot, behavior, uach, instance_tells, constant_answers }
}

/// Kick off `navigator.userAgentData.getHighEntropyValues([...])` and pump the event loop until the
/// Promise resolves (the sync surface/behavior probes can't see async client hints). `None` when the API
/// is absent or rejects.
async fn capture_uach(e: &mut Engine) -> Option<serde_json::Value> {
    let kick = r#"(function(){
      try {
        var ua = navigator.userAgentData;
        if (!ua || typeof ua.getHighEntropyValues !== "function") { print("UACH:null"); return; }
        ua.getHighEntropyValues(["architecture","bitness","brands","fullVersionList","mobile","model","platform","platformVersion","uaFullVersion","wow64"])
          .then(function(v){ print("UACH:" + JSON.stringify(v)); })
          .catch(function(e){ print("UACH:null"); });
      } catch (e) { print("UACH:null"); }
    })()"#;
    e.eval("uach-kick", kick.to_string()).ok()?;
    for _ in 0..50 {
        let _ = e.run().await;
        for line in e.take_output() {
            if let Some(rest) = line.strip_prefix("UACH:") {
                return if rest == "null" { None } else { serde_json::from_str(rest).ok() };
            }
        }
    }
    None
}

/// Boot a WORKER realm, run the surface dumper inside it (via `new Worker` + postMessage), and return
/// the set of interface names it exposes — for the Exposed-realm cross-check (Group 4).
async fn worker_interfaces() -> std::collections::BTreeSet<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
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
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(line) = out.iter().find(|l| l.starts_with("WSURFACE:")) {
            let json = &line["WSURFACE:".len()..];
            let v: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
            return v
                .get("interfaces")
                .and_then(|i| i.as_object())
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default();
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            return Default::default();
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// Bind the cases and hand back `fixtures/behavior-run.js`, which evals every `expr` under try/catch
/// and returns a JSON object keyed by case index → `{ value }` (JSON-safe result),
/// `{ threw: "<error name>" }`, or `{ unsupported }` (a non-JSON-safe result).
///
/// The runner is a FILE, not a string literal here, because `plugins/korvus-persona/capture/collect.js` evaluates the
/// very same source inside a real Chrome-151 to record what `expected` should be. Two copies would let
/// a harness difference masquerade as a behavior difference.
/// Evaluate the corpus one INTERFACE AT A TIME, restoring the page whenever a case damaged it.
///
/// The capture side (`plugins/korvus-persona/capture/collect.js`) runs each pass in a fresh frame -- its `mkFrame`
/// -- so a destructive case can never bleed into another interface's answers. This side used to
/// evaluate all 4,690 cases into one long-lived page, which was fine only for as long as no case was
/// destructive. `Document.json` has one that is: `new Document().replaceChildren()`. There is exactly
/// one document in this engine (see JsDocument), so `new Document()` is a handle to the page, and
/// once `replaceChildren` genuinely started emptying it, every later interface saw a blank page and
/// eight recorded answers in DocumentType and HTMLCollection stopped matching -- evidence lost to a
/// harness difference rather than to an engine defect.
///
/// The fingerprint is deliberately coarse: it asks only whether the page still has the structure the
/// fixture loaded with. A reload costs a parse, so it happens only when one is actually needed.
async fn run_behavior_cases(e: &mut Engine, cases: &[oracle::BehaviorCase]) -> serde_json::Value {
    // A fingerprint of the loaded fixture, compared against the one taken right after it loaded.
    // Not a hand-written list of "things that should be true": the FIRST attempt at this checked
    // documentElement/body/head and never fired, because `document.replaceChildren()` clears the
    // arena's child list while those three still answer from cached ids -- the doctype was the only
    // visible casualty. Comparing the whole shape against the original catches whatever a case broke.
    const FINGERPRINT: &str = r#"(() => { try {
        const d = document;
        return [d.childNodes.length,
                d.doctype ? d.doctype.name : "-",
                d.documentElement ? d.documentElement.tagName : "-",
                d.body ? d.body.childNodes.length : -1,
                d.head ? d.head.childNodes.length : -1].join("|");
      } catch (err) { return "err"; } })()"#;

    let fingerprint = |e: &mut Engine| -> String {
        e.eval_value("behavior-fingerprint", FINGERPRINT.to_string())
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "err".to_string())
    };
    let pristine = fingerprint(e);

    let mut merged = serde_json::Map::new();
    let mut start = 0usize;
    while start < cases.len() {
        // One interface's cases are contiguous: the corpus is read one file at a time.
        let iface = &cases[start].iface;
        let mut end = start;
        while end < cases.len() && &cases[end].iface == iface {
            end += 1;
        }
        let chunk = &cases[start..end];
        // A chunk with no awaited case returns its JSON string synchronously and never touches the
        // event loop (see `Engine::eval_value_settled`); one with awaited cases returns a promise.
        // The budget is per CHUNK and scales with how many awaited cases it holds: the runner's own
        // per-case deadline is 1500 ms, so 2 s of headroom each plus 10 s of slack is comfortably
        // above the worst case the runner can produce and still bounded.
        let awaited = chunk.iter().filter(|c| c.awaited).count();
        let budget = std::time::Duration::from_secs(10 + 2 * awaited as u64);
        let raw = e
            .eval_value_settled("behavior-oracle", behavior_runner_from(chunk, start), budget)
            .await;
        match raw {
            Ok(v) => {
                let group: serde_json::Value = v
                    .as_str()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .expect("behavior runner must return a JSON string");
                if let serde_json::Value::Object(m) = group {
                    merged.extend(m);
                }
                // SAY WHICH QUESTION NEVER SETTLED. `{ timeout: true }` matches no expectation, so the
                // case fails either way -- but a member that fails because the promise never came back
                // is a different defect from one that fails on the value, and a run that does not
                // distinguish them sends someone to fix the wrong thing.
                for (i, c) in chunk.iter().enumerate() {
                    if merged.get(&(start + i).to_string()).and_then(|v| v.get("timeout")).is_some() {
                        eprintln!("behavior TIMEOUT  {}.{}  {}", c.iface, c.member, c.expr);
                    }
                }
            }
            // NAME THE CHUNK. The predecessor of this code `.unwrap()`ed and then `.expect`ed a
            // string, so a single hung or broken chunk killed the whole harness while naming
            // nothing -- the exact failure the lone-surrogate comment above was written about.
            // A chunk that could not answer leaves its cases unrecorded, which grades them as
            // failing; that is the honest outcome and it costs one interface, not the run.
            Err(err) => {
                eprintln!("behavior chunk {iface} ({} case(s), {awaited} awaited) produced no answer: {err}", chunk.len());
            }
        }
        if fingerprint(e) != pristine {
            e.load_html(PAGE).unwrap();
            e.run().await.unwrap();
        }
        start = end;
    }
    serde_json::Value::Object(merged)
}

/// One `{id, expr, await?}` pair as the runner reads it.
///
/// `await` is emitted ONLY when the case sets it. The runner keys on `c.await === true`, so an
/// always-present `"await": false` would be harmless -- but the pairs array is also what
/// `plugins/korvus-persona/capture/collect.js` evaluates in the browser, and the two sides must be able to be compared
/// literally when someone is chasing a harness-versus-engine difference.
fn behavior_pair(id: usize, c: &oracle::BehaviorCase) -> serde_json::Value {
    let mut v = serde_json::json!({ "id": id.to_string(), "expr": c.expr });
    if c.awaited {
        v["await"] = serde_json::Value::Bool(true);
    }
    v
}

/// The runner for one chunk, with ids offset so they stay the corpus-wide indices the caller expects.
fn behavior_runner_from(cases: &[oracle::BehaviorCase], offset: usize) -> String {
    let pairs: Vec<serde_json::Value> =
        cases.iter().enumerate().map(|(i, c)| behavior_pair(offset + i, c)).collect();
    let cases_json = serde_json::to_string(&pairs).unwrap();
    format!("globalThis.__KORVUS_CASES = {cases_json};\n{RUNNER}")
}

// (`behavior_runner(cases)` stood here: the same body with the offset fixed at 0. Every caller runs
// the corpus in chunks now and passes its offset, so `behavior_runner_from(cases, 0)` is what it was.
// Deleted.)

/// Fold the raw behavior-results value into per-(interface, member) observations the report grades.
fn behavior_observations(
    cases: &[oracle::BehaviorCase],
    results: &serde_json::Value,
) -> std::collections::BTreeMap<String, std::collections::BTreeMap<String, oracle::BehaviorObs>> {
    let mut out: std::collections::BTreeMap<String, std::collections::BTreeMap<String, oracle::BehaviorObs>> =
        Default::default();
    for (i, c) in cases.iter().enumerate() {
        let r = results.get(i.to_string());
        let (ours, threw, undef) = match r {
            Some(r) if r.get("threw").and_then(|v| v.as_str()).is_some() => {
                (None, r["threw"].as_str().map(str::to_string), false)
            }
            // `undefined` is its own answer, distinct both from a value and from `null` — a case that
            // expects it passes, one that expects anything else fails.
            Some(r) if r.get("undef").and_then(|v| v.as_bool()) == Some(true) => (None, None, true),
            Some(r) if r.get("value").is_some() => (Some(r["value"].clone()), None, false),
            _ => (None, None, false), // unsupported / missing — matches nothing, so the case fails
        };
        out.entry(c.iface.clone())
            .or_default()
            .entry(c.member.clone())
            .or_default()
            .observe_full(c, ours.as_ref(), threw.as_deref(), undef);
    }
    out
}

#[ignore = "heavy: boots V8, walks the whole surface, grades every interface"]
#[tokio::test]
async fn quality() {
    let (cases, unparseable) = oracle::from_dir(&persona::shipped_artifact("oracle"));
    // A file that fails to parse would silently vanish from the corpus and hand its interface a free
    // `complete` (no cases ⇒ behavior ungraded). Fail loudly instead — this is the gate's own input.
    assert!(unparseable.is_empty(), "unparseable behavior-case files: {unparseable:?}");
    assert!(!cases.is_empty(), "no behavior cases loaded from the shipped bundle's oracle");
    let cap = capture(&cases).await;
    let raw = cap.snapshot.clone();
    let behavior = behavior_observations(&cases, &cap.behavior);
    let behavior_for = |name: &str| behavior.get(name);

    // Emit the raw snapshot for the `webapi quality` CLI (its get_snapshot shells this with
    // KORVUS_SNAPSHOT_OUT set, then reads the file — it ignores our stdout and exit-status detail).
    if let Ok(out) = std::env::var("KORVUS_SNAPSHOT_OUT") {
        let pretty = serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .unwrap_or_else(|| raw.clone());
        std::fs::write(&out, &pretty).unwrap_or_else(|e| panic!("KORVUS_SNAPSHOT_OUT write to {out} failed: {e}"));
        eprintln!("wrote snapshot -> {out}");
    }
    // Emit the behavior observations for the `webapi quality` CLI, so its per-member report carries the
    // ⑥ behavior column (the CLI can't eval; the harness hands it the tallies).
    if let Ok(out) = std::env::var("KORVUS_BEHAVIOR_OUT") {
        std::fs::write(&out, serde_json::to_string_pretty(&behavior).unwrap())
            .unwrap_or_else(|e| panic!("KORVUS_BEHAVIOR_OUT write to {out} failed: {e}"));
        eprintln!("wrote behavior obs -> {out}");
    }

    // Snapshot-only mode (the Implement-loop gate): the caller grades per-interface via the CLI from the
    // emitted snapshot+behavior and does NOT need the report-level sections (globals/tells/UA-CH/worker/
    // exposed/coherence), so this returns before them.
    //
    // IT RETURNS. It used to `std::process::exit(0)`, on the reasoning that the engine's V8/tokio worker
    // threads outlive the test function and keep the process -- and the cargo build lock -- alive.
    // Measured 2026-09-11 with output capture ON (the condition the old comment named): `return` exits in
    // 2s with `test result: ok. 1 passed` and a 2.9 MB snapshot written. The deadlock does not reproduce.
    //
    // WHY IT WAS WORTH CHECKING: a test binary that calls `exit(0)` mid-suite ends every test after it AND
    // reports success. Measured: 2 tests selected, 1 ran, no `test result` line, exit code 0 -- a caller
    // reading the exit status sees a pass. That is a silent false pass, and it gets far worse the moment
    // integration-test binaries are merged, which is why this was fixed before that landed.
    if std::env::var("KORVUS_SNAPSHOT_ONLY").is_ok() {
        assert!(!raw.is_empty(), "snapshot must be non-empty");
        std::io::Write::flush(&mut std::io::stderr()).ok();
        return;
    }

    // Typed snapshot + the three captured references (paths relative to CARGO_MANIFEST_DIR =
    // core/korvus-webapi; the brave baseline still lives with the legacy parity probe for now).
    let snap = snapshot::SurfaceSnapshot::from_json(&raw).expect("snapshot JSON parses");
    let corpus = merge::parse_corpus(&manifest().join("gen/webref"));
    assert!(!corpus.is_empty(), "no interfaces parsed from gen/webref (is the corpus vendored?)");
    let baseline: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            persona::shipped_artifact("surface"),
        )
        .expect("read the shipped surface baseline"),
    )
    .expect("parse the shipped surface baseline");
    let prof = profile::Profile::from_json(persona::SHIPPED_VALUES)
        .expect("the persona crate's embedded values parse");

    // Optional WPT (axis ⑤): fold in a dump when KORVUS_WPT_JSON points at one, else `-`. Bucketed both
    // per interface (fallback) and per member (Step 1 — per-member spec grading).
    let wpt_files = std::env::var("KORVUS_WPT_JSON")
        .ok()
        .and_then(|p| std::fs::read_to_string(&p).ok())
        .and_then(|j| wpt::parse_dump(&j).ok());
    let buckets = wpt_files.as_ref().map(|f| wpt::bucket(f));
    let member_buckets = wpt_files.as_ref().map(|f| wpt::bucket_by_member(f));
    let bucket_for = |name: &str| buckets.as_ref().and_then(|b| b.get(name));
    let members_for = |name: &str| member_buckets.as_ref().and_then(|b| b.get(name));

    // Grade every corpus interface the live surface exposes, printing the 5-axis rollup table (this is
    // the numbers a human diffs against surface_parity/nolies/... to confirm subsumption before deleting).
    let mut graded = 0usize;
    let mut verdicts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut ctor_lies: Vec<String> = Vec::new(); // subsumes `nolies` ctor half
    let mut structural: Vec<String> = Vec::new(); // subsumes `surface_parity` structural diff
    println!(
        "\n{:<28} {:<12} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "interface", "verdict", "present", "native", "shape", "value", "wpt"
    );
    println!("{}", "-".repeat(92));
    for (name, spec) in &corpus {
        if !snap.has_interface(name) {
            continue;
        }
        let r = quality::assess(name, spec, &snap, &baseline, &prof, bucket_for(name), members_for(name), behavior_for(name));
        println!(
            "{:<28} {:<12} {:>8} {:>8} {:>8} {:>8} {:>8}",
            name, r.verdict, r.rollup.present, r.rollup.native, r.rollup.shape, r.rollup.value, r.rollup.wpt
        );
        if !r.ctor.ok {
            ctor_lies.push(name.clone());
        }
        if !r.structure.matches {
            let extra = r.structure.extra_members.len();
            let miss = r.structure.missing_members.len();
            structural.push(format!("{name} (+{extra}/-{miss} members)"));
        }
        *verdicts.entry(r.verdict.clone()).or_default() += 1;
        graded += 1;
    }
    println!("\n{graded} interfaces graded (snapshot ∩ corpus)");
    for (v, c) in &verdicts {
        println!("  {v}: {c}");
    }

    // Phase A — the report-level globals scan (subsumes `automation_tells` + surface_parity's global
    // diff) and the interface-level ctor / structure axes (subsume `nolies` ctor + surface_parity).
    let globals = quality::scan_globals(&snap, &baseline);
    println!("\n=== globals scan ===");
    println!("  automation tells (window): {} {:?}", globals.automation_tells.len(), &globals.automation_tells);
    println!("  automation tells (document): {} {:?}", globals.document_tells.len(), &globals.document_tells);
    println!("  Function.prototype.toString native: {}", globals.fp_tostring_native);
    println!("  korvus-only extra globals: {}", globals.extra_globals.len());
    println!("  baseline-only missing globals: {}", globals.missing_globals.len());
    println!("\n=== ctor axis ===");
    println!("  non-native constructors: {} {:?}", ctor_lies.len(), head(&ctor_lies, 12));
    println!("\n=== structure axis ===");
    println!("  interfaces with structural diffs: {} {:?}", structural.len(), head(&structural, 12));

    // Full-surface tell scan over ALL dumped interfaces (not just the corpus intersection) — reproduces
    // surface_parity's OWN-TELLS + NAME-DEFECT scoreboard across the whole surface. Reported, not gated
    // (surface_parity tolerated these too); the affected interfaces also show as `not-native` above.
    let tells = quality::scan_tells(&snap);
    println!("\n=== full-surface tell scan ({} interfaces) ===", snap.interface_names().len());
    println!("  own-tells (native fn owns prototype/arguments/caller): {} {:?}", tells.own_tells.len(), head(&tells.own_tells, 12));
    println!("  name-defects (native fn empty name): {} {:?}", tells.name_defects.len(), head(&tells.name_defects, 12));

    // The INSTANCE axis (Track 0.8). The scan above -- and every other axis on this board -- walks
    // PROTOTYPES, which is why they all read zero while `localStorage.getItem` was a JS arrow minted
    // per access inside a Proxy `get` trap and `sheet.cssRules` was a plain Array with an `item`
    // closure stapled on. Neither member was ever on a prototype, so nothing here could see them.
    // This line reads live platform objects instead. Gated in tests/instance_tells.rs, which also
    // carries the named ratchet of the tells still outstanding.
    println!("\n=== instance axis (live platform objects) ===");
    println!(
        "  instance-tells: {} {:?}",
        cap.instance_tells.len(),
        head(&cap.instance_tells, 12)
    );

    // The PLAUSIBILITY axis (axis ⑦). Every line above -- prototype shape, nativeness, brand -- is
    // satisfied by a member that answers a constant. `MediaSource.isTypeSupported` was green on all
    // six and returned `false` for every codec on earth, because the corpus's one recorded case
    // (`isTypeSupported("x") == false`) is a question a constant `false` answers correctly. This line
    // hands each capability site several inputs and counts the ones that gave a single answer to all
    // of them. Gated in tests/constant_answers.rs, which carries the named ratchet.
    println!("\n=== plausibility axis (one answer for every input) ===");
    println!(
        "  constant-answers: {} {:?}",
        cap.constant_answers.len(),
        head(&cap.constant_answers, 12)
    );

    // Step 2 — behavioral capture (Pillar B): the oracle's `expr` cases actually RAN in Korvus; report
    // the per-member pass/total. This is the "does it work?" signal present/native/shape can't give.
    let (mut beh_pass, mut beh_total) = (0usize, 0usize);
    let mut beh_fails: Vec<String> = Vec::new();
    for (iface, members) in &behavior {
        for (member, obs) in members {
            beh_pass += obs.pass;
            beh_total += obs.total;
            if obs.pass < obs.total {
                beh_fails.push(format!("{iface}.{}", obs.fail_detail.clone().unwrap_or_else(|| member.clone())));
            }
        }
    }
    println!("\n=== behavior (oracle capture) ===");
    println!("  cases: {beh_pass}/{beh_total} passing across {} interfaces", behavior.len());
    if !beh_fails.is_empty() {
        println!("  failures: {:?}", head(&beh_fails, 12));
    }

    // UA-CH high-entropy (async client hints, universal fingerprint): the resolved getHighEntropyValues
    // must match the captured profile (nav.uaHighEntropy) — the high-entropy slice of client_hints.
    let uach_keys =
        ["architecture", "bitness", "mobile", "model", "platform", "platformVersion", "uaFullVersion", "wow64"];
    let mut uach_mismatches: Vec<String> = Vec::new();
    let uach_status = match &cap.uach {
        None => "unavailable / rejected".to_string(),
        Some(resolved) => {
            for k in uach_keys {
                match (resolved.get(k), prof.get_path(&format!("nav.uaHighEntropy.{k}"))) {
                    (Some(o), Some(w)) if o != w => uach_mismatches.push(format!("{k}: {o} != {w}")),
                    _ => {}
                }
            }
            format!("{}/{} scalars match profile", uach_keys.len() - uach_mismatches.len(), uach_keys.len())
        }
    };
    println!("\n=== UA-CH high-entropy (async) ===");
    println!("  getHighEntropyValues: {uach_status}");
    if !uach_mismatches.is_empty() {
        println!("  mismatches: {:?}", uach_mismatches);
    }

    // Group 4 — Exposed realm: each interface must live in the realm(s) its WebIDL `[Exposed]` declares.
    // A Window-only interface leaking into the worker is a tell; a Worker-exposed one missing is a gap.
    let worker_ifaces = worker_interfaces().await;
    let exposed = quality::check_exposed(&corpus, &snap, &worker_ifaces);
    println!("\n=== exposed realm (window vs worker) ===");
    println!("  korvus worker interfaces: {}", worker_ifaces.len());
    println!(
        "  Worker-exposed but missing from worker: {} {:?}",
        exposed.missing_in_worker.len(),
        head(&exposed.missing_in_worker, 12)
    );
    println!(
        "  Window-only LEAKED into worker: {} {:?}",
        exposed.leaked_to_worker.len(),
        head(&exposed.leaked_to_worker, 12)
    );

    // Phase C — cross-layer coherence (subsumes coherence.rs's positive check): the captured persona
    // must thread UA <-> navigator.platform <-> UA-CH platform <-> WebGL vendor, and UA Chrome-major <->
    // UA-CH brand/uaFullVersion <-> TLS profile, with no contradictions. Pure (no V8).
    let incoherences = spec::coherence::audit(&persona::CapturedPersona::default());
    println!("\n=== coherence audit ===");
    println!(
        "  cross-layer incoherences: {} {:?}",
        incoherences.len(),
        incoherences.iter().map(|i| i.layers).collect::<Vec<_>>()
    );

    // FLOOR only: the probe ran and the aggregator produced grades. Not a conformance gate.
    assert!(graded > 0, "no interfaces graded — the probe, corpus, or references are broken");
    // Automation tells are the one HARD stealth invariant: a `cdc_`/webdriver global is a definitive
    // bot signal that must never be present (this subsumes automation_tells.rs's core assertion).
    assert!(
        globals.automation_tells.is_empty() && globals.document_tells.is_empty(),
        "automation tell(s) present: window={:?} document={:?}",
        globals.automation_tells,
        globals.document_tells
    );
    assert!(globals.fp_tostring_native, "Function.prototype.toString is not native (a stealth tell)");
    // The captured persona must carry no contradiction its bundle has not accounted for. Not "no
    // contradiction at all": this bundle is honestly mixed and its manifest says so in eight entries,
    // each with a reason and a fix. An UNDECLARED one is the thing that must never ship, because a
    // device that disagrees with itself is a definitive bot signal on its own.
    let unaccounted = spec::coherence::unaccounted(&persona::CapturedPersona::default());
    assert!(
        unaccounted.is_empty(),
        "captured persona has an unaccounted cross-layer incoherence: {unaccounted:?} \
         (the full audit, declared entries included, was: {incoherences:?})"
    );
}

// --- Phase C coherence, standalone + pure (no V8, no --ignored) — fully reproduces coherence.rs so it
// can be deleted. The heavy `quality` probe above also asserts the positive case; these run in the plain
// `cargo test -p korvus-webapi` pass and add the negative control. ---

/// The captured persona threads every layer (UA <-> navigator.platform <-> UA-CH <-> TLS <-> WebGL) with
/// no contradiction.
#[test]
fn captured_persona_has_no_unaccounted_incoherence() {
    // THIS TEST USED TO ASSERT THE AUDIT WAS EMPTY, AND IT PASSED FOR THE WRONG REASON.
    // `transport_profile()` returned an id DERIVED from the UA's Chrome major, so the audit's
    // "UA major vs TLS profile" check compared the UA against a number computed from the UA. It
    // could not fail, and this test recorded that it did not fail as though it were evidence.
    //
    // The bundle declares the preset now, so the comparison has two independent sides -- and it
    // immediately reports the truth: the UA says 151, the ClientHello is 137, because the pinned
    // wreq-util ships nothing newer. That is real and is written down as `known_incoherence
    // "tls-version"` in the bundle manifest, with a reason and a fix.
    //
    // So the assertion that means something is UNACCOUNTED, not EMPTY: every contradiction must be
    // one the bundle has declared. A new one fails here and stops the engine at boot.
    let p = persona::CapturedPersona::default();
    let unaccounted = spec::coherence::unaccounted(&p);
    assert!(
        unaccounted.is_empty(),
        "the shipped persona has an incoherence its bundle does not account for: {unaccounted:?}\n\
         Add a [[known_incoherence]] with a reason and a fix, or close the gap."
    );
    // The TLS check compares the WIRE the audit can see, not the preset's name (coherence.rs): the shipped persona
    // measured its own `trust_anchors`/`h2`/`quic`, so there is no real gap to raise -- the ClientHello is
    // JA4-identical to the real browser -- and the raw audit no longer manufactures a `UA<->TLS.profile` finding from
    // the preset's trailing integer. The negative control that a broken check would slip past is
    // `frankenstein_persona_is_flagged`: it rides a bare old preset with no measured wire and must still be flagged.
    let raw = spec::coherence::audit(&p);
    assert!(
        !raw.iter().any(|i| i.layers.contains("TLS")),
        "the shipped persona's wire is measured, so the raw audit must not cry wolf over the preset name: {raw:?}"
    );
}

/// Mac UA but a Windows navigator.platform + Windows UA-CH platform + a Chrome-120 TLS profile under a
/// Chrome-151 UA — three cross-layer contradictions the Coherence Engine must catch (the negative control).
struct Franken(persona::CapturedPersona);
impl spec::Persona for Franken {
    fn navigator(&self) -> spec::NavigatorProfile {
        let mut n = self.0.navigator();
        n.platform = "Win32".into();
        n
    }
    fn screen(&self) -> spec::ScreenProfile {
        self.0.screen()
    }
    fn transport_profile(&self) -> spec::TlsProfile {
        spec::TlsProfile { id: "chrome_120".into(), platform: "windows".into(), ..Default::default() }
    }
    fn navigator_extra(&self) -> spec::NavigatorExtra {
        self.0.navigator_extra()
    }
    fn client_hints(&self) -> spec::ClientHints {
        let mut h = self.0.client_hints();
        h.platform = "Windows".into();
        h
    }
    fn device_oracle_json(&self) -> String {
        self.0.device_oracle_json()
    }
}

#[test]
fn frankenstein_persona_is_flagged() {
    let issues = spec::coherence::audit(&Franken(persona::CapturedPersona::default()));
    let layers: Vec<&str> = issues.iter().map(|i| i.layers).collect();
    assert!(layers.contains(&"UA<->navigator.platform"), "got: {issues:?}");
    assert!(layers.contains(&"UA<->UA-CH.platform"), "got: {issues:?}");
    assert!(layers.contains(&"UA<->TLS.profile"), "got: {issues:?}");
}

// --- Phase B constructability (boots the engine; reproduces constructable.rs so it can be deleted).
// Interfaces constructable in real Chrome must be `new`-able here with a NATIVE ctor, not throw "is not
// a constructor" -- a behavioral tell (`new Image()` is extremely common). ---
#[tokio::test]
async fn common_interfaces_are_constructable() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    // A document, because `Image`/`Audio`/`Option` are element FACTORIES -- `new Image()` is
    // `document.createElement("img")` with two attribute defaults. They used to answer with a bare
    // shell object that had no `tagName` and was not an HTMLImageElement, which is why this test
    // passed without one; a window in a browser always has a document, so running with none was
    // asserting browser behaviour in a state no browser is ever in.
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval(
        "c",
        r#"
        const ok = [];
        const mk = {
            Image: () => new Image(), Audio: () => new Audio(), Option: () => new Option(),
            Path2D: () => new Path2D(), EventSource: () => new EventSource("http://x/"),
            OffscreenCanvas: () => new OffscreenCanvas(1, 1),
            OfflineAudioContext: () => new OfflineAudioContext(1, 44100, 44100),
            AudioContext: () => new AudioContext(),
        };
        for (const [n, f] of Object.entries(mk)) {
            let r;
            try { r = (f() != null) && globalThis[n].toString().includes("[native code]"); }
            catch (e) { r = false; }
            ok.push(n + "=" + r);
        }
        print(ok.join(" "));
        // The three legacy factories are a stronger claim than "constructable": each is a DISTINCT
        // function sharing the element interface's prototype, so what comes back is a real element.
        print("factory=" + [
            Image.prototype === HTMLImageElement.prototype,
            Audio.prototype === HTMLAudioElement.prototype,
            Option.prototype === HTMLOptionElement.prototype,
            new Image() instanceof HTMLImageElement,
            new Image(2, 3).width === 2,
            new Option("a", "b").value === "b",
            document.createElement("img").tagName === "IMG",
        ].join(","));
    "#
        .into(),
    )
    .unwrap();
    e.run().await.unwrap();
    assert_eq!(
        e.take_output(),
        vec![
            "Image=true Audio=true Option=true Path2D=true EventSource=true OffscreenCanvas=true OfflineAudioContext=true AudioContext=true".to_string(),
            "factory=true,true,true,true,true,true,true".to_string(),
        ]
    );
}
