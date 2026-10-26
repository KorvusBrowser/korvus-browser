//! THE CHECK `kb_slots` CANNOT BE: the bridge holder's own key list, read out of a running realm
//! (invariant I3).
//!
//! # Why a ratchet was not enough, and is still not
//!
//! `scripts/freeze.sh`'s `kb_slots` counts `KB.__x =` spellings in `.js` files and in the kernel's
//! Rust STRING LITERALS. That is a precise measurement of a real thing, and it has read 0 since the
//! last JS file went. It is also structurally blind to the shape the tree actually had:
//! `bridge_set(scope, "__x", …)` — a Rust call, naming the slot as an ordinary argument — matches
//! nothing it looks for. Four slots lived under that 0 for months, and the declaration had to carry
//! I3 as the one invariant it could not claim.
//!
//! Widening the regex would not have fixed it, for the reason `freeze.sh`'s own rule 3 records about a
//! different rule: the FORMS are unbounded. A slot can be written through a helper one call deeper, by
//! a name built at run time, from a crate the ratchet does not scan, or by `set_prop` on a holder
//! fetched some other way — which is exactly how `worker_recv.rs` mounts the three that remain, and
//! `kb_slots` does not see those either.
//!
//! So this asks the OBJECT instead of the source. Every route ends at one place: a property of
//! `globalThis[Symbol.for("korvus.bridge")]`. Whatever put a slot there, it is there, and
//! `Object.getOwnPropertyNames` on the holder finds it. There is nothing to spell around.
//!
//! MEASURED BOTH WAYS BEFORE IT WAS COMMITTED. Dropping the `__swScriptLoaded` row failed the service
//! realm; adding a slot from Rust under a name BUILT AT RUN TIME (`"__snea" + "kySlot"`) failed all
//! three worker realms — while `kb_slots` stayed at 0 through that same mutation and reported nothing.
//!
//! # What it asserts
//!
//! One row per (realm, slot) in `tests/fixtures/bridge_slots.manifest`, in BOTH directions: an
//! undeclared slot fails, and a declared slot a realm no longer has fails too, so deleting a mount
//! deletes its row in the same commit. The WINDOW realm has no rows — and its assertion is stronger
//! than "no slots", because it has no holder at all: `Symbol(korvus.bridge)` is absent from the page's
//! global rather than filtered out of it, which is why `binding::hidden`'s two rewritten intrinsics
//! could be deleted with it.

use engine::Engine;

const MANIFEST: &str = include_str!("fixtures/bridge_slots.manifest");

/// The declared slots of one realm, sorted. Rows are `<realm>\t<slot>\t<mount>\t<why>`.
fn declared(realm: &str) -> Vec<String> {
    let mut out: Vec<String> = MANIFEST
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let mut it = l.split('\t');
            let r = it.next()?;
            let slot = it.next()?;
            // A row with no reason is not a declaration. The four columns are the contract.
            let _mount = it.next().filter(|s| !s.trim().is_empty())?;
            let _why = it.next().filter(|s| !s.trim().is_empty())?;
            (r == realm).then(|| slot.to_string())
        })
        .collect();
    out.sort();
    out
}

/// The JS that reads one realm's holder keys. `Object.getOwnPropertyNames`, not a `for…in`: the holder
/// is a null-prototype object in every realm that has one, and own keys are the question anyway.
const KEYS: &str = "(() => { const h = globalThis[Symbol.for('korvus.bridge')]; \
                   return h ? Object.getOwnPropertyNames(h).sort().join(',') : 'NO-HOLDER'; })()";

/// One realm's reported keys as a list. `NO-HOLDER` and an empty holder are the same answer here — no
/// slots — and a DIFFERENT answer to [`the_window_realm_has_no_bridge_holder`], which asserts the
/// stronger of the two.
fn split(reported: &str) -> Vec<String> {
    if reported == "NO-HOLDER" || reported.is_empty() {
        return Vec::new();
    }
    reported.split(',').map(str::to_string).collect()
}

/// Drive a page that registers `sources` by URL and evaluates `driver`, pumped until `RESULT:`.
async fn run(sources: &[(&str, &str)], driver: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    let entries: Vec<String> = sources
        .iter()
        .map(|(url, src)| {
            format!("{}: {}", serde_json::to_string(url).unwrap(), serde_json::to_string(src).unwrap())
        })
        .collect();
    e.eval("worker-scripts", format!("globalThis.__workerScripts = {{ {} }};", entries.join(", "))).unwrap();
    e.run().await.unwrap();
    e.eval("driver", driver.to_string()).unwrap();
    let mut out = Vec::new();
    let start = std::time::Instant::now();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("RESULT:")) {
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    out
}

fn result_of(out: &[String]) -> String {
    out.iter()
        .find(|l| l.starts_with("RESULT:"))
        .map(|l| l["RESULT:".len()..].to_string())
        .unwrap_or_else(|| format!("NO RESULT -- full output: {out:?}"))
}

async fn window_slots() -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body>x</body></html>").unwrap();
    e.eval("keys", format!("print({KEYS});")).unwrap();
    e.run().await.unwrap();
    split(e.take_output().first().map(String::as_str).unwrap_or(""))
}

async fn dedicated_keys() -> String {
    let src = format!("self.postMessage({KEYS});");
    let out = run(
        &[("./keys.js", &src)],
        "const w = new Worker('./keys.js'); w.onmessage = (e) => print('RESULT:' + e.data);",
    )
    .await;
    result_of(&out)
}

async fn shared_keys() -> String {
    let src = format!("const k = {KEYS}; self.onconnect = (e) => {{ e.ports[0].postMessage(k); }};");
    let out = run(
        &[("./skeys.js", &src)],
        "const w = new SharedWorker('./skeys.js'); w.port.onmessage = (e) => print('RESULT:' + e.data); w.port.start();",
    )
    .await;
    result_of(&out)
}

async fn service_keys() -> String {
    // Captured at top level, where a holder mounted before the script ran would be seen, and posted from
    // the `install` handler, which is the first moment the page can be told anything.
    let src = format!(
        "const k = {KEYS}; \
         self.addEventListener('install', () => {{ new BroadcastChannel('bridge-keys').postMessage(k); }});"
    );
    let out = run(
        &[("./swkeys.js", &src)],
        "const c = new BroadcastChannel('bridge-keys'); \
         c.onmessage = (e) => print('RESULT:' + e.data); \
         navigator.serviceWorker.register('./swkeys.js');",
    )
    .await;
    result_of(&out)
}

/// THE WINDOW REALM HAS NO BRIDGE AT ALL — not an empty one, and not a filtered one.
///
/// This is the assertion that closes the enforcement clause. Before, the answer here was a
/// holder carrying `__curScript`, `__loadFired` and `__bodyHandlerAttr`, kept off
/// `Object.getOwnPropertySymbols(window)` by two intrinsics this engine had replaced. All three moved
/// (a V8 Private, an `OpState` field, and a bind that had had no reader since W5.1 L19), so nothing
/// mints a holder and there is no symbol to hide.
#[tokio::test]
async fn the_window_realm_has_no_bridge_holder() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body>x</body></html>").unwrap();
    e.eval("keys", format!("print('keys=' + {KEYS});")).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec!["keys=NO-HOLDER"]);
    assert!(declared("window").is_empty(), "the window realm must declare no bridge slots");
}

/// NO WORKER REALM HAS A BRIDGE EITHER — the same stronger assertion as the window's.
///
/// A worker's holder carried `__flushConnects` (and a ServiceWorker's `__swScriptLoaded`), for JS strings
/// appended to the worker's script; `worker.rs`'s `after_top_level` makes both calls from Rust now, so no
/// holder is minted. It was page-visible: `Object.getOwnPropertySymbols(self)` listed it, where Chrome's
/// three worker globals list nothing (F-worker-no-bridge).
#[tokio::test]
async fn a_dedicated_worker_has_no_bridge_holder() {
    assert_eq!(dedicated_keys().await, "NO-HOLDER");
    assert!(declared("worker").is_empty(), "a dedicated worker must declare no bridge slots");
}

/// A SharedWorker -- the realm `__flushConnects` was written for -- has none.
#[tokio::test]
async fn a_shared_worker_has_no_bridge_holder() {
    assert_eq!(shared_keys().await, "NO-HOLDER");
    assert!(declared("shared").is_empty(), "a shared worker must declare no bridge slots");
}

/// A ServiceWorker -- which also had `__swScriptLoaded` -- has none.
#[tokio::test]
async fn a_service_worker_has_no_bridge_holder() {
    assert_eq!(service_keys().await, "NO-HOLDER");
    assert!(declared("service").is_empty(), "a service worker must declare no bridge slots");
}

/// MEMBERSHIP IS DERIVED, NEVER HAND-CURATED — the same contract `js_strings.manifest` runs under.
///
/// `KORVUS_UPDATE_BRIDGE_SLOTS=1 cargo test -p korvus-runtime --test all -- bridge_slots::` boots all
/// four realms, reads each holder's own keys, and rewrites the manifest from what it FOUND. The reason
/// column of every row whose (realm, slot) is unchanged is carried over verbatim; a row that is new
/// gets `REASON-REQUIRED`, which fails [`the_manifest_rows_are_well_formed`] until a human writes why
/// the slot is allowed to exist. A row the realms no longer have simply does not come back.
///
/// So a row cannot be added to this file to silence the gate: writing the row is not what makes the
/// assertions pass — the realm having the slot is — and a row with no reason fails on its own.
#[tokio::test]
async fn regenerate_the_manifest_when_asked() {
    if std::env::var_os("KORVUS_UPDATE_BRIDGE_SLOTS").is_none() {
        return;
    }
    let mut found: Vec<(String, String)> = Vec::new();
    for (realm, slots) in [
        ("window", window_slots().await),
        ("worker", split(&dedicated_keys().await)),
        ("shared", split(&shared_keys().await)),
        ("service", split(&service_keys().await)),
    ] {
        for s in slots {
            found.push((realm.to_string(), s));
        }
    }
    // The mount and reason of every row this tree already declares, keyed by (realm, slot).
    let mut prior: Vec<((String, String), (String, String))> = Vec::new();
    for line in MANIFEST.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let c: Vec<&str> = line.split('\t').collect();
        if c.len() == 4 {
            prior.push(((c[0].into(), c[1].into()), (c[2].into(), c[3].into())));
        }
    }
    let mut out: String = MANIFEST.lines().take_while(|l| l.starts_with('#')).collect::<Vec<_>>().join("\n");
    out.push('\n');
    for key in &found {
        let (mount, why) = prior
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| ("REASON-REQUIRED".into(), "REASON-REQUIRED".into()));
        out.push_str(&format!("{}\t{}\t{}\t{}\n", key.0, key.1, mount, why));
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/bridge_slots.manifest");
    std::fs::write(path, out).expect("write the manifest");
    eprintln!("wrote {} rows to {path}", found.len());
}

/// The manifest itself is well formed: four non-empty tab-separated columns, a known realm, a slot
/// spelled `__name`, no duplicate (realm, slot), and no `REASON-REQUIRED` left unanswered.
///
/// Without this a row could lose its reason column and still "declare" a slot, which is the failure
/// mode every other manifest in this tree guards against by the same means.
#[test]
fn the_manifest_rows_are_well_formed() {
    let mut seen: Vec<(String, String)> = Vec::new();
    for (n, line) in MANIFEST.lines().enumerate() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        assert_eq!(cols.len(), 4, "line {}: want 4 tab-separated columns, got {}", n + 1, cols.len());
        assert!(
            matches!(cols[0], "window" | "worker" | "shared" | "service"),
            "line {}: unknown realm {:?}",
            n + 1,
            cols[0]
        );
        assert!(cols[1].starts_with("__"), "line {}: {:?} is not a `__` slot name", n + 1, cols[1]);
        for (i, c) in cols.iter().enumerate() {
            assert!(!c.trim().is_empty(), "line {}: column {} is empty", n + 1, i + 1);
            assert!(
                *c != "REASON-REQUIRED",
                "line {}: column {} is REASON-REQUIRED -- a regenerated row needs a human to say why \
                 this slot is allowed to exist",
                n + 1,
                i + 1
            );
        }
        let key = (cols[0].to_string(), cols[1].to_string());
        assert!(!seen.contains(&key), "line {}: duplicate row {:?}", n + 1, key);
        seen.push(key);
    }
    // NO COUNT IS PINNED HERE. `scripts/freeze.sh`'s `bridge_slots_live` ratchet counts these rows and
    // only ever lets the number fall, which is the tree's one mechanism for that; a second number
    // written down by hand is the "count pin" the contract forbids, and it would have to be
    // edited by anyone deleting a slot — turning a fall into a chore instead of a result. (It asserted
    // the rows were NOT empty while the worker realms had slots; now no realm has
    // one, and each realm's own test above asserts it has no holder at all.)
}
