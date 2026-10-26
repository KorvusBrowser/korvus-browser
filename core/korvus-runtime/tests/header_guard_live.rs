//! CONF GUARD1 — Fetch's header GUARD, measured against the file that IS the measurement.
//!
//! `tools/sync/data/header_guard.json` was read off Google Chrome 150.0.7871.46 by
//! a header-guard probe page. This
//! test runs that probe's script in a real window and asserts the engine answers what the browser
//! answered — so the JSON is the single source and neither this file nor `modules/fetch/headers.rs`
//! carries a second copy of the name lists.
//!
//! THE CONTROL. `x-custom` must be SET under guard `none`, in every run. If it ever reads DROP the
//! probe is broken and the whole table is meaningless, so it is asserted separately and by name
//! ([[prove-the-check-can-fail]]).
//!
//! WHY THE `request-no-cors` EXPECTATION IS NAME-ONLY HERE. The real rule is value-aware — measured:
//! `content-type: application/json` drops where `content-type: text/plain` sticks — and the engine
//! asks `fetch_global::no_cors_safelisted`. This test does not re-implement that; it uses the
//! probe's own values, every one of which passes the value half for the four safelisted names, so
//! over THIS name set the expected answer collapses to "is the name safelisted". The value half has
//! its own case below.
use engine::Engine;

/// The measurement. Same `include_str!` the engine uses, so a re-probe moves both at once.
const GUARD_JSON: &str = include_str!("../../../tools/sync/data/header_guard.json");

/// The probe's script, verbatim in spirit: build one `Headers` per guard, `set(name, value)` on
/// each, then `get(name)` — `null` means the guard DROPPED it.
const PROBE: &str = r#"
(() => {
  const VALS = {"accept-encoding":"gzip","content-type":"text/plain","range":"bytes=0-9",
                "access-control-request-method":"GET","te":"trailers"};
  const G = {
    "none": new Headers(),
    "request": new Request("/x", {headers: new Headers()}).headers,
    "request-no-cors": new Request("/x", {mode: "no-cors", headers: new Headers()}).headers,
    "response": new Response("", {headers: new Headers()}).headers,
  };
  const out = {};
  for (const g of Object.keys(G)) {
    out[g] = {};
    for (const n of NAMES) {
      try { G[g].set(n, VALS[n] || "1"); out[g][n] = G[g].get(n) === null ? "DROP" : "SET"; }
      catch (e) { out[g][n] = "THROW:" + e.name; }
    }
  }
  // The value half of the `request-no-cors` safelist, and the guard a plain `new Headers()` gets.
  const nc = new Request("/x", {mode: "no-cors", headers: new Headers()}).headers;
  nc.set("content-type", "application/json");
  out.value_half = {
    "no-cors content-type application/json": nc.get("content-type"),
    "no-cors accept 200 bytes": (() => { const h = new Request("/x", {mode: "no-cors"}).headers; h.set("accept", "a".repeat(200)); return h.get("accept"); })(),
    "a copy of a guarded list is guard none": (() => { const h = new Headers(new Request("/x", {headers: {}}).headers); h.set("cookie", "x"); return h.get("cookie"); })(),
    "a Request clone keeps its guard": (() => { const h = new Request("/x", {headers: {}}).clone().headers; h.set("cookie", "x"); return h.get("cookie"); })(),
    "a Response clone keeps its guard": (() => { const h = new Response("", {headers: {}}).clone().headers; h.set("set-cookie", "a"); return h.get("set-cookie"); })(),
    "the guard filters the INIT, not only later mutations": new Request("/x", {headers: {cookie: "x"}}).headers.get("cookie"),
    "and it keeps what it does not forbid": new Request("/x", {headers: {cookie: "x", "x-custom": "1"}}).headers.get("x-custom"),
    "the body's content-type is NOT exempt under no-cors": new Request("/x", {method: "POST", mode: "no-cors", body: new Blob(["x"], {type: "application/json"})}).headers.get("content-type"),
    "the same body in cors mode keeps it": new Request("/x", {method: "POST", body: new Blob(["x"], {type: "application/json"})}).headers.get("content-type"),
  };
  // `append` judges the COMBINED value; `set` does not.
  const NC = () => new Request("/x", {mode: "no-cors"}).headers;
  const A = "a".repeat(100), B = "b".repeat(100);
  const len = (h, n) => { const v = h.get(n); return v === null ? null : v.length; };
  out.combine = {
    "one 100-byte append": (() => { const h = NC(); h.append("accept", A); return len(h, "accept"); })(),
    "two 100-byte appends": (() => { const h = NC(); h.append("accept", A); h.append("accept", B); return len(h, "accept"); })(),
    "two 10-byte appends": (() => { const h = NC(); h.append("accept", "aaaaaaaaaa"); h.append("accept", "bbbbbbbbbb"); return len(h, "accept"); })(),
    "accept-language en then (": (() => { const h = NC(); h.append("accept-language", "en"); h.append("accept-language", "("); return h.get("accept-language"); })(),
    "content-type text/plain then application/json": (() => { const h = NC(); h.append("content-type", "text/plain"); h.append("content-type", "application/json"); return h.get("content-type"); })(),
    "set does not combine": (() => { const h = NC(); h.set("accept", A); h.set("accept", B); return len(h, "accept"); })(),
    "guard none combines freely": (() => { const h = new Headers(); h.append("accept", A); h.append("accept", B); return len(h, "accept"); })(),
  };
  return JSON.stringify(out);
})()
"#;

fn doc() -> serde_json::Value {
    serde_json::from_str(GUARD_JSON).expect("tools/sync/data/header_guard.json is JSON")
}

fn list(d: &serde_json::Value, k: &str) -> Vec<String> {
    d.get(k)
        .and_then(|a| a.as_array())
        .unwrap_or_else(|| panic!("header_guard.json has `{k}`"))
        .iter()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect()
}

/// Every name the file mentions, plus the two invented ones the prefix rule was proven with. The
/// probe's own candidate list is wider; these are the ones the file has an ANSWER for.
fn probe_names(d: &serde_json::Value) -> Vec<String> {
    let mut v = list(d, "forbidden_request_header_names");
    v.extend(list(d, "cors_safelisted_request_headers"));
    v.extend(list(d, "forbidden_response_header_names"));
    // The prefix evidence: invented names, so no fixed list can accidentally cover them.
    v.extend(["proxy-foo".to_string(), "sec-foo".to_string(), "sec-fetch-mode".to_string()]);
    // The control, and a name measured SET everywhere but no-cors.
    v.extend(["x-custom".to_string(), "range".to_string()]);
    v.sort();
    v.dedup();
    v
}

/// What the browser answered, derived from the file — not from the spec and not from this engine.
fn expected(d: &serde_json::Value, guard: &str, name: &str) -> &'static str {
    let forbidden_req = list(d, "forbidden_request_header_names").iter().any(|f| f == name)
        || list(d, "forbidden_request_header_prefixes").iter().any(|p| name.starts_with(p.as_str()));
    match guard {
        "none" => "SET",
        "request" => {
            if forbidden_req {
                "DROP"
            } else {
                "SET"
            }
        }
        "request-no-cors" => {
            if list(d, "cors_safelisted_request_headers").iter().any(|f| f == name) {
                "SET"
            } else {
                "DROP"
            }
        }
        "response" => {
            if list(d, "forbidden_response_header_names").iter().any(|f| f == name) {
                "DROP"
            } else {
                "SET"
            }
        }
        other => panic!("no such guard: {other}"),
    }
}

async fn run_probe() -> serde_json::Value {
    let d = doc();
    let names = probe_names(&d);
    let names_js = serde_json::to_string(&names).expect("the name list serialises");
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    e.eval("header_guard_probe", format!("const NAMES = {names_js}; print('HG:' + {PROBE});")).unwrap();
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(line) = out.iter().find(|l| l.starts_with("HG:")) {
            return serde_json::from_str(&line[3..]).expect("the probe's JSON parses");
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            panic!("the header-guard probe never answered within 20s; output: {out:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// The four guards' name tables, every cell against `tools/sync/data/header_guard.json`.
#[tokio::test]
async fn the_four_guards_answer_what_chrome_answered() {
    let d = doc();
    let got = run_probe().await;

    // THE CONTROL FIRST. A run where this is not SET has measured nothing.
    assert_eq!(
        got["none"]["x-custom"], "SET",
        "CONTROL: `x-custom` must stick on a plain `new Headers()`. It did not, so the probe is broken and \
         every other row below is meaningless -- do not read them."
    );

    let mut wrong: Vec<String> = Vec::new();
    for guard in ["none", "request", "request-no-cors", "response"] {
        for name in probe_names(&d) {
            let want = expected(&d, guard, &name);
            let have = got[guard][&name].as_str().unwrap_or("<absent>");
            if have != want {
                wrong.push(format!("{guard}/{name}: engine {have}, Chrome {want}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{} guard cells disagree with the browser:\n  {}", wrong.len(), wrong.join("\n  "));
}

/// The half of the rule a name list cannot hold: the `request-no-cors` safelist is VALUE-aware, a
/// copy of a guarded list is guard `none`, a clone keeps its guard, and the guard filters the INIT
/// rather than only later mutations. Every expectation measured on Chrome 150.0.7871.46
/// (the CASES section).
#[tokio::test]
async fn the_guard_rules_a_name_list_cannot_hold() {
    let got = run_probe().await;
    let v = &got["value_half"];
    let null = serde_json::Value::Null;
    assert_eq!(v["no-cors content-type application/json"], null, "the no-cors safelist is value-aware: application/json is not a safelisted content-type");
    assert_eq!(v["no-cors accept 200 bytes"], null, "the no-cors safelist caps a value at 128 bytes");
    assert_eq!(v["a copy of a guarded list is guard none"], "x", "CONTROL: `new Headers(other)` is always guard `none`");
    assert_eq!(v["a Request clone keeps its guard"], null, "a Request clone's headers are still guard `request`");
    assert_eq!(v["a Response clone keeps its guard"], null, "a Response clone's headers are still guard `response`");
    assert_eq!(v["the guard filters the INIT, not only later mutations"], null, "a Request built with a `cookie` in its init drops it at FILL time, not merely on a later set()");
    assert_eq!(v["and it keeps what it does not forbid"], "1", "CONTROL: the same init's `x-custom` survives");
    assert_eq!(v["the body's content-type is NOT exempt under no-cors"], null, "the body's Content-Type goes through the guard like any other append");
    assert_eq!(v["the same body in cors mode keeps it"], "application/json", "CONTROL: only the no-cors guard drops it");
}

/// `append` under `request-no-cors` judges the COMBINED value -- what `get(name)` would answer
/// afterwards -- and `set` does not, because `set` replaces. Measured on Chrome 150.0.7871.46
/// (`header-guard-probe.html`, the CASES J block); `headers-no-cors.any.js` loses seven subtests
/// without it, each one a pair of values that passes the 128-byte cap alone and fails together.
#[tokio::test]
async fn append_is_judged_on_the_combined_value_and_set_is_not() {
    let got = run_probe().await;
    let c = &got["combine"];
    assert_eq!(c["one 100-byte append"], 100, "CONTROL: 100 bytes is under the cap and sticks");
    assert_eq!(c["two 10-byte appends"], 22, "CONTROL: a short pair combines to 22 and both are kept");
    assert_eq!(c["guard none combines freely"], 202, "CONTROL: guard `none` judges nothing");
    assert_eq!(c["two 100-byte appends"], 100, "the SECOND append is dropped: the combined 202 bytes is over the 128-byte cap");
    assert_eq!(c["accept-language en then ("], "en", "the combined `en, (` carries a CORS-unsafe byte");
    assert_eq!(c["content-type text/plain then application/json"], "text/plain", "the combined content-type is not a safelisted essence");
    assert_eq!(c["set does not combine"], 100, "`set` REPLACES, so its value is judged alone");
}
