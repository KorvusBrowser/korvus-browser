//! LIVE one-table gate for the WORKER `WorkerNavigator` (Phase 3 Part 2/3; docs/archive/goal-phase3/briefs/
//! worker-navigator.md).
//!
//! Spawns a REAL worker isolate -- whose spawn now builds the worker `WorkerNavigator` prototype from
//! the same one-table `NAVIGATOR` (worker selector, 23 members) via
//! `webapi::install::live::install_worker_navigator` and points the worker's running `navigator` at it -- and
//! asserts the RUNNING worker navigator is byte-identical (member names, order, descriptor shape) AND
//! value-identical (persona scalars, the intricate objects) to the captured Brave worker surface
//! (`core/korvus-webapi/tests/fixtures/brave-151.worker.surface.json`). This is the proof the worker
//! install reproduces the surface the generated worker shell used to only HINT at (it had the
//! interface object but no `navigator` instance at all) -- the precondition for dropping the shell and
//! declaring `alias "WorkerNavigator"` on the Navigator table.
//!
//! It also proves a SharedWorker (which now inherits the page persona too) still exposes a real
//! `WorkerNavigator` after the shell is gone -- the migration-safety check the brief demands, since the
//! worker shell is a single global file every worker realm shares.

use engine::Engine;
use spec::Persona;

fn artifact(role: &str) -> String {
    std::fs::read_to_string(persona::shipped_artifact(role)).unwrap()
}

/// The ordered `name:kind:...` describe lines the worker install must produce for the WorkerNavigator
/// prototype, from the captured worker surface, with the non-enumerable `constructor` at the seat
/// that capture gives it.
fn expected_worker_proto() -> Vec<String> {
    let fx: serde_json::Value =
        serde_json::from_str(&artifact("worker-surface")).unwrap();
    let mut out: Vec<String> = fx["interfaces"]["WorkerNavigator"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            let n = m["name"].as_str().unwrap();
            match m["kind"].as_str().unwrap() {
                "accessor" => format!("{n}:accessor:e=1:c=1:s={}", m["set"].as_bool().unwrap_or(false) as u8),
                "method" => format!("{n}:data:e=1:c=1:w=1"),
                k => panic!("unexpected kind {k}"),
            }
        })
        .collect();
    // AT THE WORKER CAPTURE'S SEAT, which is what this line was waiting for. It used
    // to append `constructor`, because `ctorIndex` was a window-only field and `WorkerNavigator`'s
    // member list is NOT `Navigator`'s -- the whole reason the two names exist -- so the borrow FP12
    // bridged the realms with declined here and the placement was the installer's default. The
    // worker artifacts were re-read from the four browsers and this interface is the sharpest thing
    // in them: the device seats `constructor` strictly INSIDE `WorkerNavigator`, after the twelfth
    // member, with `hid`, `serial`, `usb`, `storage` and `gpu` behind it -- appending it was wrong
    // on the one worker object a fingerprinting script actually reads.
    let at = fx["interfaces"]["WorkerNavigator"]["ctorIndex"]
        .as_u64()
        .map(|n| n as usize)
        .unwrap_or(out.len())
        .min(out.len());
    out.insert(at, "constructor:data:e=0:c=1:w=1".to_string());
    out
}

/// Build an engine (real persona), register a worker script by URL (`__workerScripts` seam), eval a
/// driver, and pump the cross-thread event loop until a line with `tag:` appears; returns that line's
/// payload. Mirrors `tests/worker.rs`'s `run_worker`.
async fn run_worker_probe(scripts: &[(&str, &str)], driver: &str, tag: &str) -> String {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    let entries: Vec<String> = scripts
        .iter()
        .map(|(url, src)| format!("{}: {}", serde_json::to_string(url).unwrap(), serde_json::to_string(src).unwrap()))
        .collect();
    e.eval("worker-scripts", format!("globalThis.__workerScripts = {{ {} }};", entries.join(", "))).unwrap();
    e.run().await.unwrap();
    e.eval("worker-driver", driver.to_string()).unwrap();
    let prefix = format!("{tag}:");
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(line) = out.iter().find(|l| l.starts_with(&prefix)) {
            return line[prefix.len()..].to_string();
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            panic!("worker did not post {tag} within 20s; output: {out:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// The probe a dedicated worker runs: describe its `navigator` prototype + read a value sample, and
/// RETURN it as one JSON string (the driver's `self.postMessage(<probe>)` wrapper ships it back).
const DEDICATED_PROBE: &str = r#"
(() => {
  const proto = Object.getPrototypeOf(navigator);
  const desc = Object.getOwnPropertyNames(proto).map((n) => {
    const d = Object.getOwnPropertyDescriptor(proto, n);
    return (d.get || d.set)
      ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
      : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
  }).join("\n");
  let newThrew = false;
  try { new WorkerNavigator(); } catch (e) { newThrew = e instanceof TypeError; }
  const uaGet = Object.getOwnPropertyDescriptor(proto, "userAgent").get;
  const out = {
    typeofNav: typeof navigator,
    typeofWN: typeof WorkerNavigator,
    proto: desc,
    ua: navigator.userAgent,
    plat: navigator.platform,
    hc: navigator.hardwareConcurrency,
    dm: navigator.deviceMemory,
    langs: JSON.stringify(navigator.languages),
    lang: navigator.language,
    ctor: navigator.constructor.name,
    isInstance: navigator instanceof WorkerNavigator,
    newThrew: newThrew,
    uaNative: uaGet.toString().includes("[native code]"),
    ownNames: JSON.stringify(Object.getOwnPropertyNames(navigator)),
    gpuTag: Object.prototype.toString.call(navigator.gpu),
    gpuSingleton: navigator.gpu === navigator.gpu,
    uaDataIsNull: navigator.userAgentData === null,
    uaDataPlat: navigator.userAgentData ? navigator.userAgentData.platform : null,
    brave: typeof navigator.brave,
    gpc: navigator.globalPrivacyControl,
    onLine: navigator.onLine,
  };
  return JSON.stringify(out);
})()
"#;

/// BYTE + VALUE identical: the running worker `navigator`'s prototype is the capture's 23-member
/// WorkerNavigator surface (member for member, in order, with the WebIDL descriptor shape), and the
/// values are the persona's (scalars live from `HostPersona`) / the recording's (intricate objects).
#[tokio::test]
async fn live_worker_navigator_is_byte_and_value_identical_to_the_capture() {
    let json = run_worker_probe(
        &[("./nav-probe.js", &format!("self.postMessage({DEDICATED_PROBE});"))],
        "const w = new Worker('./nav-probe.js'); w.onmessage = (ev) => print('WNAV:' + ev.data);",
        "WNAV",
    )
    .await;
    // The worker posts the probe's own JSON string; the driver's `self.postMessage(<probe>)` wrapper
    // means the probe's returned string is what arrives -- parse it.
    let got: serde_json::Value = serde_json::from_str(&json).expect("worker navigator probe is valid JSON");

    let p = persona::CapturedPersona::default();
    let nav = p.navigator();

    // ---- BYTE: prototype shape ----
    let proto_lines: Vec<&str> = got["proto"].as_str().unwrap().lines().collect();
    assert_eq!(
        proto_lines,
        expected_worker_proto(),
        "the worker navigator's prototype != the captured WorkerNavigator surface (worker install must reproduce it)",
    );

    // ---- presence / identity ----
    assert_eq!(got["typeofNav"], "object", "typeof navigator");
    assert_eq!(got["typeofWN"], "function", "typeof WorkerNavigator");
    assert_eq!(got["ctor"], "WorkerNavigator", "navigator.constructor.name");
    assert_eq!(got["isInstance"], true, "navigator instanceof WorkerNavigator");
    assert_eq!(got["newThrew"], true, "new WorkerNavigator() must throw TypeError");
    assert_eq!(got["uaNative"], true, "userAgent getter must be [native code]");
    assert_eq!(got["ownNames"], "[]", "worker navigator has no own props (all on the prototype)");

    // ---- VALUE: persona scalars (same identity as the window) ----
    assert_eq!(got["ua"].as_str().unwrap(), nav.user_agent, "userAgent");
    assert_eq!(got["plat"].as_str().unwrap(), nav.platform, "platform");
    assert_eq!(got["hc"].as_f64().unwrap(), nav.hardware_concurrency as f64, "hardwareConcurrency");
    assert_eq!(got["dm"].as_f64().unwrap(), nav.device_memory, "deviceMemory");
    assert_eq!(
        got["langs"].as_str().unwrap(),
        serde_json::to_string(&nav.languages).unwrap(),
        "languages",
    );
    assert_eq!(got["lang"].as_str().unwrap(), nav.languages.first().cloned().unwrap_or_default(), "language");

    // ---- VALUE: intricate objects answer, not null ----
    assert_eq!(got["gpuTag"], "[object GPU]", "navigator.gpu is a branded GPU instance");
    assert_eq!(got["gpuSingleton"], true, "navigator.gpu must be a per-navigator singleton");
    assert_eq!(got["uaDataIsNull"], false, "navigator.userAgentData must not be null");
    assert_eq!(got["uaDataPlat"].as_str().unwrap(), p.client_hints().platform, "userAgentData.platform");
    // Chrome 150's WorkerNavigator has neither (Brave 151, the persona until 2026-10-16, had both).
    assert_eq!(got["brave"], "undefined", "navigator.brave (not a Chrome 150 member)");
    assert!(got.get("gpc").is_none_or(|v| v.is_null()), "globalPrivacyControl (not a Chrome 150 member): {:?}", got.get("gpc"));
    assert_eq!(got["onLine"], true, "onLine");
}

/// MIGRATION SAFETY: a SharedWorker still exposes a real `WorkerNavigator` (and a `navigator` whose
/// constructor is `WorkerNavigator`) after the shared worker shell is dropped -- it now inherits the
/// page persona too, so its live install runs exactly as a dedicated worker's does. Without the persona
/// wiring, removing the shared shell (a single global file every worker realm loads) would leave the
/// SharedWorker with no navigator interface at all.
#[tokio::test]
async fn shared_worker_still_exposes_worker_navigator() {
    let got = run_worker_probe(
        &[(
            "./sw-nav.js",
            "self.onconnect = (e) => { const p = e.ports[0]; p.onmessage = () => \
             p.postMessage(typeof navigator + '|' + typeof WorkerNavigator + '|' + \
             (navigator.constructor && navigator.constructor.name) + '|' + navigator.userAgent); };",
        )],
        "const sw = new SharedWorker('./sw-nav.js', 'nav'); \
         sw.port.onmessage = (e) => print('SWNAV:' + e.data); sw.port.postMessage('go');",
        "SWNAV",
    )
    .await;
    let parts: Vec<&str> = got.split('|').collect();
    assert_eq!(parts[0], "object", "SharedWorker: typeof navigator ({got})");
    assert_eq!(parts[1], "function", "SharedWorker: typeof WorkerNavigator ({got})");
    assert_eq!(parts[2], "WorkerNavigator", "SharedWorker: navigator.constructor.name ({got})");
    let p = persona::CapturedPersona::default();
    assert_eq!(parts[3], p.navigator().user_agent, "SharedWorker: navigator.userAgent reads the page persona ({got})");
}
