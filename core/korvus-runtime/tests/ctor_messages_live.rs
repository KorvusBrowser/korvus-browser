//! THE INTERFACE OBJECT'S TWO MESSAGES, against real Chrome, over the WHOLE global, in
//! BOTH realms.
//!
//! `korvus-webapi/tests/installer_parity.rs` proves the generated registry's interface objects are
//! their rows, on a bare `JsRuntime`. It cannot see the hand `interface!` tables, the live installs,
//! or a real dedicated Worker — so the ten hand constructor bodies W7.1 corrected (`FontFace()`,
//! `VideoFrame()`, `MediaRecorder()`, `RTCPeerConnection()`, … all answered Chrome's sentence WITHOUT
//! its `Failed to construct 'X': ` prefix) had no gate at all. This is that gate, and it covers every
//! uppercase-initial function global of both realms rather than a list someone remembered to extend.
//!
//! # The fixture is a browser recording, not an expectation someone wrote
//!
//! `fixtures/chrome-152-ctor-messages.json` is Chrome 152.0.7977.82, headed on the Wayland desktop
//! (the persona's capture), no DevTools, against a 127.0.0.1 loopback page, 2026-10-16: for every own
//! uppercase-initial global that is a function, `new X()` and `X()`, in the page (955 globals) and
//! inside a real dedicated Worker reached from it (295). The probe below is the same code. Chrome's answers are three shapes,
//! and which one it gives is WebIDL, not chance:
//!
//!   interface declares no constructor   new X() -> Failed to construct 'X': Illegal constructor
//!                                           X() -> Illegal constructor
//!   declares one, N args required       new X() -> Failed to construct 'X': N argument(s) required,
//!                                                  but only 0 present.
//!                                           X() -> Failed to construct 'X': Please use the 'new'
//!                                                  operator, this DOM object constructor cannot be
//!                                                  called as a function.
//!   declares one, none required         new X() -> an instance;  X() -> the same "Please use…"
//!
//! # The pin is a MANIFEST of the names that still differ, not a count
//!
//! Goal invariant I5: the pinned truth is a sorted manifest. `fixtures/ctor_message_gaps.manifest`
//! names every interface whose pair this engine does not yet answer Chrome's way, one
//! `realm interface` per line, with the reason class in a comment header. Two assertions, and they
//! bite in BOTH directions:
//!
//!   * a name NOT in the manifest that differs is a REGRESSION — the gate fails and names it;
//!   * a name IN the manifest that now AGREES is progress that must be recorded — the gate fails and
//!     tells you to drop the line, so the manifest can never quietly over-state the gap.
//!
//! `KORVUS_UPDATE_CTOR_GAPS=1 cargo test -p korvus-runtime --test all -- ctor_messages_live::`
//! rewrites it; a shrinking manifest is the number this unit and its successors move.
use engine::Engine;
use std::collections::{BTreeMap, BTreeSet};

const CHROME: &str = include_str!("fixtures/chrome-152-ctor-messages.json");
const MANIFEST: &str = include_str!("fixtures/ctor_message_gaps.manifest");

/// Asks both call shapes of every own uppercase-initial function global. Realm-agnostic on purpose:
/// the window realm and a worker realm run this same source, so a divergence between them is the
/// engine's and never the probe's.
const PROBE: &str = r#"(() => {
  const out = {};
  const names = Object.getOwnPropertyNames(globalThis).filter((n) => /^[A-Z]/.test(n));
  for (const n of names) {
    let v;
    try { v = globalThis[n]; } catch (e) { continue; }
    let isfn; try { isfn = typeof v === 'function'; } catch (e) { continue; }
    if (!isfn) continue;
    // A getter that throws, a Proxy, an exception object whose `message` has a receiver check: every
    // read here is guarded, because one unguarded throw would empty the whole sweep.
    const msg = (e) => { try { return e && e.message !== undefined ? String(e.message) : String(e); } catch (x) { return '<unreadable>'; } };
    const row = {};
    try { const o = new v(); row.new = 'object:' + Object.prototype.toString.call(o); }
    catch (e) { row.new = 'throw:' + msg(e); }
    try { const o = v(); row.call = 'value:' + Object.prototype.toString.call(o); }
    catch (e) { row.call = 'throw:' + msg(e); }
    out[n] = row;
  }
  return JSON.stringify(out);
})()"#;

type Pairs = BTreeMap<String, (String, String)>;

fn pairs_of(v: &serde_json::Value) -> Pairs {
    v.as_object()
        .expect("a realm's rows are an object")
        .iter()
        .map(|(k, r)| {
            (
                k.clone(),
                (
                    r["new"].as_str().unwrap_or_default().to_string(),
                    r["call"].as_str().unwrap_or_default().to_string(),
                ),
            )
        })
        .collect()
}

async fn window_pairs() -> Pairs {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    let s = e.eval_value("ctor-msg-window", PROBE.to_string()).unwrap();
    pairs_of(&serde_json::from_str(s.as_str().expect("the probe returns a JSON string")).unwrap())
}

/// A REAL dedicated Worker isolate, reached from a page, not a second window realm: the worker global
/// is assembled by the same installer with the `Worker` filter (goal invariant I8) and nothing else in
/// the tree asks it these two questions.
async fn worker_pairs() -> Pairs {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    let body = format!("self.postMessage({PROBE});");
    let reg = format!(
        "globalThis.__workerScripts = {{ './ctor-msg.js': {} }};",
        serde_json::to_string(&body).unwrap()
    );
    e.eval("ctor-msg-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval(
        "ctor-msg-driver",
        "globalThis.__ctorMsg = null; const w = new Worker('./ctor-msg.js'); w.onmessage = (ev) => { globalThis.__ctorMsg = ev.data; print('CTORMSG:done'); };".into(),
    )
    .unwrap();
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("CTORMSG:")) {
            break;
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(60), "the worker probe never answered: {out:?}");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let s = e.eval_value("ctor-msg-read", "globalThis.__ctorMsg".into()).unwrap();
    pairs_of(&serde_json::from_str(s.as_str().expect("the worker probe returns a JSON string")).unwrap())
}

fn manifest_rows() -> BTreeSet<String> {
    MANIFEST.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_string).collect()
}

#[tokio::test]
async fn every_interface_object_answers_chromes_two_messages() {
    let chrome: serde_json::Value = serde_json::from_str(CHROME).unwrap();
    let mine = vec![("window", window_pairs().await), ("worker", worker_pairs().await)];

    let mut gaps: BTreeSet<String> = BTreeSet::new();
    let mut detail: BTreeMap<String, String> = BTreeMap::new();
    for (realm, got) in &mine {
        let want = pairs_of(&chrome[realm]);
        // Only names BOTH have. An interface Chrome ships and the capture bundle does not is not this
        // gate's business (the bundle is the denominator), and one the engine has that Chrome does not
        // is a surface question `worker_surface`/`quality` already own.
        for (name, w) in &want {
            let Some(g) = got.get(name) else { continue };
            if g == w {
                continue;
            }
            let row = format!("{realm} {name}");
            detail.insert(
                row.clone(),
                format!("  {realm} {name}\n    chrome new : {}\n    engine new : {}\n    chrome call: {}\n    engine call: {}", w.0, g.0, w.1, g.1),
            );
            gaps.insert(row);
        }
    }

    if std::env::var("KORVUS_UPDATE_CTOR_GAPS").is_ok() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ctor_message_gaps.manifest");
        let head = std::fs::read_to_string(path)
            .map(|s| s.lines().take_while(|l| l.starts_with('#')).collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();
        std::fs::write(path, format!("{head}\n{}\n", gaps.iter().cloned().collect::<Vec<_>>().join("\n"))).unwrap();
        eprintln!("wrote {} gap rows to {path}", gaps.len());
        return;
    }

    let pinned = manifest_rows();
    let regressed: Vec<&String> = gaps.difference(&pinned).collect();
    let fixed: Vec<&String> = pinned.difference(&gaps).collect();
    let mut bad = String::new();
    if !regressed.is_empty() {
        bad.push_str(&format!(
            "{} interface object(s) answer something Chrome 152 does not, and are not pinned:\n{}\n",
            regressed.len(),
            regressed.iter().map(|r| detail[*r].clone()).collect::<Vec<_>>().join("\n")
        ));
    }
    if !fixed.is_empty() {
        bad.push_str(&format!(
            "{} pinned gap(s) now MATCH Chrome -- drop them from tests/fixtures/ctor_message_gaps.manifest:\n  {}\n",
            fixed.len(),
            fixed.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n  ")
        ));
    }
    assert!(bad.is_empty(), "{bad}\n(KORVUS_UPDATE_CTOR_GAPS=1 rewrites the manifest; a line may only be REMOVED by fixing the engine)");
}
