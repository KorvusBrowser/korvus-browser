//! THE RECEIVER SWEEP — the grader for receiver checks, and the count their rollout is measured
//! against.
//!
//! # What it asks
//!
//! For every member every table declares, on a real page: what does the member do when it is reached
//! through a receiver that is not an instance of its interface? A browser throws
//! `TypeError: Illegal invocation` for all but two classes (a promise-returning member rejects
//! instead; a `[LegacyLenientThis]` member answers `undefined`), and an object whose members answer
//! for ANY receiver is not a platform object — it is a fingerprint as much as a correctness gap.
//!
//! W7 (`cfcde5fc`) ran this sweep BY HAND over the 2,399 rows `tools/status/data/layers.json` declared
//! `stub` and found 157 answering; the number then had to be re-derived by hand to see it move. It is
//! a test now, it sweeps EVERY row and not only the stubs, and it prints its classification — so the
//! number is re-measured on every gate run and a regression names the rows that caused it.
//!
//! # How a row that still answers is CLASSIFIED, without a list
//!
//! Three ledgers, all recorded by the one member mount, decide it:
//!
//! * `behavior_rt::layers_json()` — every row the installer mounted. A member NOT in it is one the
//!   installer never mounted, i.e. **a V8 intrinsic** the capture recorded and `layers.json` calls a
//!   row (`Array.prototype.push`, `String.prototype.trim` — generic in Chrome too, so this is a
//!   ledger inaccuracy and not a tell).
//! * `behavior_rt::receiver_json()` — the receiver policy `install::receiver::policy` read off each
//!   row, and when there is none, WHICH of the reasons applied (`none:static`, `none:kind`,
//!   `none:global`, `none:generic`, `none:sealed`, `none:absent`). Those are the rows that are
//!   **WebIDL-generic on purpose**, each with the reason attached.
//! * The two together: a row the installer mounted, whose policy says `throw`/`reject`, that still
//!   answers, is one where what the page sees is NOT what the installer mounted — **a second writer
//!   overwrote it** after the installer ran. W7 confirmed two of those by hand
//!   (`HTMLLinkElement.sheet`, `Document.elementFromPoint`: masked JS bodies in
//!   `korvus-runtime/src/lib.rs` string literals). W4/W5 deleted them; `second_writers.manifest` is
//!   empty and the mask ops themselves are gone.
//!
//! "It went down" is not the deliverable; the classified remainder is. So the assertions here are on
//! the CLASSIFICATION being total — every answering row must fall in one of the three — and on the
//! two headline cases W9.17 was raised for.

use engine::Engine;
use std::collections::BTreeMap;

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e
}

fn declared() -> serde_json::Value {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/status/data/layers.json");
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))).unwrap()
}

fn map_of(json: String) -> BTreeMap<String, String> {
    serde_json::from_str::<serde_json::Value>(&json)
        .unwrap()
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
        .collect()
}

/// The probe, run inside the page. One line per row: `Iface::member<TAB>outcome`.
///
/// The receiver is `{}` — the case every measured defect used and the one the bundle's oracle asked
/// 334 times. A method is called with its own `length` of `undefined` arguments so the WebIDL
/// argument-count guard cannot answer FIRST and hide the receiver question (Chrome checks the
/// receiver before the arity; a row that threw "1 argument required" would otherwise be scored as
/// correctly refusing).
const PROBE: &str = r#"(labels => {
    const kind = v => v === null ? "null" : v === undefined ? "undefined"
        : (typeof v === "object" || typeof v === "function") ? Object.prototype.toString.call(v) : typeof v;
    // A row whose policy is `reject` answers a PROMISE, and "is a promise" is not the question --
    // the question is whether it REJECTS. Stash every thenable so SETTLE can await it; a resolved
    // promise from a refusal is a hole this sweep used to score as a refusal (CONF TY1.3).
    globalThis.__rsPending = [];
    const out = [];
    for (const label of labels) {
        const i = label.indexOf("::");
        const iface = label.slice(0, i), member = label.slice(i + 2);
        let r;
        try {
            const I = globalThis[iface];
            if (typeof I !== "function" || !I.prototype || typeof I.prototype !== "object") { r = "SKIP:no-interface"; }
            else {
                const d = Object.getOwnPropertyDescriptor(I.prototype, member);
                if (!d) r = "SKIP:not-own";
                else if (d.get) {
                    const v = d.get.call({}); r = "ANSWER:" + kind(v);
                    if (v && typeof v.then === "function") globalThis.__rsPending.push([label, v]);
                }
                else if (d.set) { d.set.call({}, undefined); r = "ANSWER:set"; }
                else if (typeof d.value === "function") {
                    const f = d.value;
                    const v = f.apply({}, new Array(Math.min(f.length, 8)).fill(undefined));
                    r = "ANSWER:" + kind(v);
                    if (v && typeof v.then === "function") globalThis.__rsPending.push([label, v]);
                } else r = "SKIP:value";
            }
        } catch (e) { r = "THROW:" + (e && e.name) + ":" + (e && e.message); }
        out.push(label + "\t" + r);
    }
    return out.join("\n");
})"#;

/// THE SECOND HALF OF THE PROBE, and the reason this sweep can be believed at all.
///
/// `PROBE` is synchronous, so all it can say about a promise-returning refusal is "[object
/// Promise]". That is not the browser's rule. The rule is that the promise REJECTS, and until
/// 2026-09-23 this file scored a refusal that RESOLVED exactly like one that rejected -- a lane
/// made refusals resolve on purpose and the sweep still passed (CONF TY1.3). So every thenable the
/// probe collected is awaited here.
///
/// `Promise.allSettled` never rejects, and the whole batch is raced against one timeout rather than
/// each promise separately: a genuinely pending promise must not turn a 5,000-row sweep into a
/// stopwatch.
const SETTLE: &str = r#"(async () => {
    const p = globalThis.__rsPending || [];
    if (!p.length) return "";
    const settled = await Promise.race([
        Promise.allSettled(p.map(x => x[1])),
        new Promise(res => setTimeout(() => res(null), 5000)),
    ]);
    if (!settled) return "__TIMEOUT__";
    return p.map((x, i) => x[0] + "\t" + (settled[i].status === "rejected"
        ? "REJECT:" + ((settled[i].reason && settled[i].reason.name) || "?")
        : "RESOLVE:" + Object.prototype.toString.call(settled[i].value))).join("\n");
})()"#;

struct Sweep {
    /// `Iface::member` -> the probe's outcome string.
    outcome: BTreeMap<String, String>,
    layers: BTreeMap<String, String>,
    receiver: BTreeMap<String, String>,
    /// `Iface::member` -> `REJECT:<name>` / `RESOLVE:<kind>`, for the rows that answered a promise.
    settled: BTreeMap<String, String>,
}

async fn sweep() -> Sweep {
    let mut e = page().await;
    let d = declared();
    // Every row every table declares, both realms, minus the symbol-keyed ones (a `@@iterator` row is
    // not reachable by string name and is the shared `Array.prototype` function anyway).
    let mut labels: Vec<String> = Vec::new();
    for realm in ["window", "worker"] {
        let Some(ifaces) = d["layers"][realm].as_object() else { continue };
        for (iface, members) in ifaces {
            let Some(members) = members.as_object() else { continue };
            for member in members.keys() {
                if member.starts_with("@@") || member.starts_with("Symbol") {
                    continue;
                }
                labels.push(format!("{iface}::{member}"));
            }
        }
    }
    labels.sort();
    labels.dedup();
    assert!(labels.len() > 5000, "only {} rows to sweep -- layers.json is not being read", labels.len());

    let src = format!("({PROBE})({})", serde_json::to_string(&labels).unwrap());
    let got = e.eval_value("receiver-sweep", src).unwrap().as_str().unwrap().to_string();
    let outcome: BTreeMap<String, String> = got
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    // Await everything the probe collected. `eval_value_settled` pumps the loop; a plain
    // `eval_value` hands back `null` for a promise, which is how the hole stayed open.
    let settled_src = SETTLE.to_string();
    let raw = e
        .eval_value_settled("receiver-sweep-settle", settled_src, std::time::Duration::from_secs(20))
        .await
        .expect("settle")
        .as_str()
        .unwrap_or("")
        .to_string();
    assert_ne!(raw, "__TIMEOUT__", "a refusal promise never settled -- the sweep cannot classify it");
    let settled: BTreeMap<String, String> = raw
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Sweep { outcome, layers: map_of(webapi::behavior_rt::layers_json()), receiver: map_of(webapi::behavior_rt::receiver_json()), settled }
}

/// THE NUMBER. Sweep every declared row with a foreign receiver, classify every one that answers,
/// and fail only if a row answers for a reason none of the three ledgers can explain.
#[tokio::test]
async fn every_row_that_answers_a_foreign_receiver_is_classified() {
    let s = sweep().await;

    let on = webapi::behavior_rt::receiver_check_enabled();
    let (mut probed, mut threw, mut skipped) = (0usize, 0usize, 0usize);
    // `reject` gets its OWN counter. Folding it into `threw` is what made a row moving
    // throw -> reject invisible: TY1 changed 59 rows and this sweep's numbers were byte-identical.
    let mut rejected = 0usize;
    // A row whose policy is `reject` whose promise RESOLVED. Not a refusal, and named, never tallied.
    let mut resolved_refusals: Vec<String> = Vec::new();
    // W7's own denominator, kept so the two measurements stay comparable: of the rows
    // `tools/status/data/layers.json` declares `stub`, how many answer? That sweep was run by hand at
    // `018ce677` and found 157.
    let mut stub_answering = 0usize;
    let mut intrinsic: Vec<String> = Vec::new();
    let mut second_writer: Vec<String> = Vec::new();
    let mut generic: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut unexplained: Vec<String> = Vec::new();

    for (label, out) in &s.outcome {
        if out.starts_with("SKIP:") {
            skipped += 1;
            continue;
        }
        probed += 1;
        if out.starts_with("THROW:") {
            threw += 1;
            continue;
        }
        // A promise-returning member REJECTS rather than throwing -- that IS the browser's answer, so
        // it is not a row that "answers" in the sense this sweep counts. BUT "it is a promise" is
        // not the rule and was never the rule: the promise has to REJECT. Scoring `[object Promise]`
        // alone made this sweep pass while refusals RESOLVED (CONF TY1.3), so the settlement decides
        // it now, and `reject` gets its own counter instead of being folded into `threw`.
        if s.receiver.get(label).map(String::as_str) == Some("reject") && out == "ANSWER:[object Promise]" {
            match s.settled.get(label).map(String::as_str) {
                Some(v) if v.starts_with("REJECT:") => {
                    rejected += 1;
                    continue;
                }
                // A refusal that RESOLVES is the hole. It is not a refusal, and it is not classified
                // away -- it falls through to the classification below and is reported by name.
                Some(_) => resolved_refusals.push(label.clone()),
                None => resolved_refusals.push(label.clone()),
            }
        }
        // `[LegacyLenientThis]`: `undefined` is Chrome's answer, for the getter and the setter.
        if s.receiver.get(label).map(String::as_str) == Some("lenient") {
            threw += 1;
            continue;
        }
        match (s.layers.get(label), s.receiver.get(label).map(String::as_str)) {
            // The installer never mounted it: V8 owns the property and Chrome is generic on it too.
            (None, _) => intrinsic.push(label.clone()),
            // The installer mounted it AND read a real policy off the row, yet the page sees
            // something that answers: what the page sees is not what the installer mounted.
            (Some(_), Some("throw")) | (Some(_), Some("reject")) => second_writer.push(label.clone()),
            // The row has no policy, and the ledger says which of the reasons applied.
            (Some(_), Some(r)) if r.starts_with("none:") => {
                generic.entry(Box::leak(r.to_string().into_boxed_str())).or_default().push(label.clone())
            }
            // Mounted, but the installer recorded no receiver policy at all: the D2 stub owns it (it
            // carries its own check) -- so a row that answers here has been overwritten too.
            (Some(_), None) => second_writer.push(label.clone()),
            _ => unexplained.push(label.clone()),
        }
        if s.layers.get(label).map(String::as_str) == Some("stub") {
            stub_answering += 1;
        }
    }

    let answering = intrinsic.len() + second_writer.len() + generic.values().map(Vec::len).sum::<usize>() + unexplained.len();
    eprintln!(
        "receiver sweep (check {}): {probed} rows probed, {skipped} skipped, {threw} threw, \
         {rejected} rejected, {answering} answered \
         ({stub_answering} of them rows layers.json declares `stub` -- W7's denominator, 157 at 018ce677)",
        if on { "ON" } else { "OFF" }
    );
    // THE ASSERTION THIS FILE WAS MISSING. A promise-returning refusal that RESOLVES is not a
    // refusal; it answers for a foreign receiver as surely as a row that returns a value. Until
    // 2026-09-23 it was scored as a refusal, and a lane proved the hole by making refusals resolve
    // on purpose -- the sweep passed (CONF TY1.3).
    assert!(
        resolved_refusals.is_empty(),
        "{} row(s) declare policy `reject` and answered a promise that RESOLVED, which is not a \
         refusal -- the browser rejects. Rows: {:?}",
        resolved_refusals.len(),
        resolved_refusals.iter().take(20).collect::<Vec<_>>()
    );
    eprintln!("  V8 intrinsic (the installer never mounted it): {}", intrinsic.len());
    // The SAME bucket means two different things either side of the rollout, and saying which is the
    // point of a counting gate: with the check compiled OFF it is "rows that would change answer when
    // it is switched on"; with it ON it is "rows a second writer took over after the installer
    // mounted them", because the installer's own function would have refused.
    eprintln!(
        "  {}: {}",
        if on { "second writer (mounted with a policy, overwritten since)" } else { "WOULD CHANGE ANSWER when the check is switched on" },
        second_writer.len()
    );
    for (why, rows) in &generic {
        eprintln!("  WebIDL-generic on purpose [{why}]: {}", rows.len());
        for l in rows.iter().take(12) {
            eprintln!("      {l}");
        }
    }
    // By INTERFACE, because a second writer is a whole file redefining a whole family and the tally
    // is what points at the file to delete -- 30 individual member names do not.
    let mut by_iface: BTreeMap<&str, usize> = BTreeMap::new();
    for l in &second_writer {
        *by_iface.entry(l.split_once("::").map(|p| p.0).unwrap_or(l)).or_default() += 1;
    }
    let mut tally: Vec<(&&str, &usize)> = by_iface.iter().collect();
    tally.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (iface, n) in tally {
        eprintln!("    {} x{n}: {iface}", if on { "second writer" } else { "would change" });
    }

    assert!(probed > 3000, "only {probed} rows were reachable -- the probe is not finding the prototypes");
    assert!(
        unexplained.is_empty(),
        "{} row(s) answer a foreign receiver and NONE of the three ledgers explains why -- that is a \
         hole in the classification, not a smaller number:\n{}",
        unexplained.len(),
        unexplained.iter().take(25).cloned().collect::<Vec<_>>().join("\n")
    );
}

/// THE TWO CASES W9.17 WAS RAISED FOR, and the canaries that say the check is not too strict.
///
/// Every expectation below is Chrome 150.0.7871.46's own answer, measured with
/// `--headless=new --dump-dom`, not this engine's answer written down. See
/// `install::receiver`'s module comment for the full transcript.
#[tokio::test]
async fn the_measured_defects_answer_what_chrome_answers() {
    let mut e = page().await;
    let src = r#"(() => {
        const g = (p, n) => Object.getOwnPropertyDescriptor(p, n).get;
        const s = (p, n) => Object.getOwnPropertyDescriptor(p, n).set;
        const r = [];
        const ask = (label, f) => { try { const v = f();
            r.push(label + " = returned " + Object.prototype.toString.call(v)); }
            catch (e) { r.push(label + " = " + (e && e.name) + ": " + (e && e.message)); } };
        // For a question whose ANSWER is the value rather than its kind.
        const askv = (label, f) => { try { r.push(label + " = value " + String(f())); }
            catch (e) { r.push(label + " = " + (e && e.name) + ": " + (e && e.message)); } };
        // W4.a's case, and L17's shape on canvas.
        ask("Navigator.sendBeacon {}", () => Navigator.prototype.sendBeacon.call({}, "/x"));
        ask("CRC2D.fillRect {}", () => CanvasRenderingContext2D.prototype.fillRect.call({}, 0, 0, 1, 1));
        ask("CRC2D.canvas {}", () => g(CanvasRenderingContext2D.prototype, "canvas").call({}));
        // A reflected attribute pair, and a plain DOM method.
        ask("HTMLImageElement.alt get {}", () => g(HTMLImageElement.prototype, "alt").call({}));
        ask("HTMLImageElement.alt set {}", () => s(HTMLImageElement.prototype, "alt").call({}, "x"));
        ask("Element.getAttribute {}", () => Element.prototype.getAttribute.call({}, "id"));
        // An ordinary event handler throws; `[LegacyLenientThis]` answers undefined.
        ask("HTMLElement.onclick get {}", () => g(HTMLElement.prototype, "onclick").call({}));
        ask("Document.onreadystatechange get {}", () => g(Document.prototype, "onreadystatechange").call({}));
        ask("HTMLElement.onmouseenter get {}", () => g(HTMLElement.prototype, "onmouseenter").call({}));
        // THE CANARIES: a GENUINE instance must keep answering. These are the two suites that caught
        // an over-strict check before (`anim_live`, `rtc_graph_live`), in one line each.
        ask("GENUINE document.timeline.currentTime", () => typeof document.timeline.currentTime);
        ask("GENUINE img.alt on a real element", () => document.createElement("img").alt);
        ask("GENUINE ctx.fillRect on a real context", () => {
            const c = document.createElement("canvas").getContext("2d"); c.fillRect(0, 0, 1, 1); return "ok"; });
        ask("GENUINE navigator.sendBeacon is callable", () => typeof navigator.sendBeacon);
        // THE SHARED MEMBERS. `SVGElement.prototype`'s `tabIndex`/`style`/`focus`/`dataset` run
        // `HTMLElement.prototype`'s bodies. They were COPIED from it once (`09k-svg.js`, then
        // `install::live::foreign`) and every copy carried a check aimed at HTMLElement.prototype,
        // which a real `<svg>` element is not under: all seven threw on a genuine instance until a
        // re-aim pass followed the copies (coverage.sh: 5608 verified -> 5601). Since then
        // they are SVGElement's own rows, checked against its own prototype from the start.
        const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
        ask("GENUINE svg.tabIndex", () => svg.tabIndex);
        ask("GENUINE svg.style", () => svg.style);
        ask("GENUINE svg.dataset", () => svg.dataset);
        ask("GENUINE svg.focus()", () => svg.focus());
        // ...and the donor's own copy refuses an SVG element, which is the OTHER half of re-aiming
        // and is Chrome's answer too.
        ask("HTMLElement.tabIndex get svg", () => g(HTMLElement.prototype, "tabIndex").call(svg));
        askv("IDENTITY SVGElement.tabIndex.get === HTMLElement.tabIndex.get",
            () => g(SVGElement.prototype, "tabIndex") === g(HTMLElement.prototype, "tabIndex"));
        return r.join("\n");
    })()"#;
    let got = e.eval_value("receiver-headline", src.to_string()).unwrap().as_str().unwrap().to_string();
    eprintln!("{got}");

    let want: &[(&str, &str)] = &[
        // Chrome: TypeError: Illegal invocation.
        ("Navigator.sendBeacon {}", "TypeError: Illegal invocation"),
        ("CRC2D.fillRect {}", "TypeError: Illegal invocation"),
        ("CRC2D.canvas {}", "TypeError: Illegal invocation"),
        ("HTMLImageElement.alt get {}", "TypeError: Illegal invocation"),
        ("HTMLImageElement.alt set {}", "TypeError: Illegal invocation"),
        ("Element.getAttribute {}", "TypeError: Illegal invocation"),
        ("HTMLElement.onclick get {}", "TypeError: Illegal invocation"),
        // Chrome: returned [object Undefined] -- `[LegacyLenientThis]`.
        ("Document.onreadystatechange get {}", "returned [object Undefined]"),
        ("HTMLElement.onmouseenter get {}", "returned [object Undefined]"),
        // A genuine instance is untouched -- the whole point of the weaker check.
        ("GENUINE document.timeline.currentTime", "returned [object String]"),
        ("GENUINE img.alt on a real element", "returned [object String]"),
        ("GENUINE ctx.fillRect on a real context", "returned [object String]"),
        ("GENUINE navigator.sendBeacon is callable", "returned [object String]"),
        // Chrome 150.0.7871.46 on a real `<svg>`: Number, CSSStyleDeclaration, DOMStringMap,
        // undefined -- and `HTMLElement.prototype.tabIndex`'s getter called on it throws
        // `Illegal invocation`, with the two interfaces' getters `!==` (all four measured).
        ("GENUINE svg.tabIndex", "returned [object Number]"),
        ("GENUINE svg.style", "returned [object CSSStyleDeclaration]"),
        ("GENUINE svg.dataset", "returned [object DOMStringMap]"),
        ("GENUINE svg.focus()", "returned [object Undefined]"),
        ("HTMLElement.tabIndex get svg", "TypeError: Illegal invocation"),
        ("IDENTITY SVGElement.tabIndex.get === HTMLElement.tabIndex.get", "value false"),
    ];
    let mut bad = Vec::new();
    for (label, expect) in want {
        let line = got.lines().find(|l| l.starts_with(&format!("{label} = "))).unwrap_or("<missing>");
        if !line.ends_with(expect) {
            bad.push(format!("{label}: want `{expect}`, got `{line}`"));
        }
    }
    if !webapi::behavior_rt::receiver_check_enabled() {
        // While the check is compiled OFF the engine cannot answer the first half; the canaries and
        // the lenient pair still must, because neither depends on the check.
        let canaries: Vec<String> = bad.iter().filter(|b| b.starts_with("GENUINE")).cloned().collect();
        assert!(canaries.is_empty(), "a GENUINE instance stopped answering:\n{}", canaries.join("\n"));
        eprintln!("receiver check OFF: {} of {} Chrome answers not yet matched", bad.len(), want.len());
        return;
    }
    assert!(bad.is_empty(), "the engine disagrees with Chrome 150.0.7871.46:\n{}", bad.join("\n"));
}
