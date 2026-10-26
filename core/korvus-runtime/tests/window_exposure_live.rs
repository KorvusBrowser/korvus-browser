//! A NAME ARRIVES WITH ITS EXPOSURE CONDITION (extended by FP5.9) — asserted at two
//! origins, four bundles, over the WHOLE window.
//!
//! # The rule this file asserts
//!
//! > `window.hasOwnProperty(name)` is true exactly when the worn capture records the name **and** the
//! > name's exposure condition holds for THIS DOCUMENT'S ORIGIN.
//!
//! Two facts of different kinds, and the defect this file exists for was the second one going
//! unstated. FP5 seated 33 own globals with the shape their browser gives them and installed every one
//! of them unconditionally; twelve of them do not exist at all on a non-secure origin, and the WPT
//! core tier says so about one of the twelve (`fetch/fetch-later/non-secure.window.js`). Asserting
//! that one file would cover one name, so this file asserts the rule instead.
//!
//! FP5.7 closed those twelve at the hand-install sites. FP5.9 found the other **231**: the registry's
//! interface objects — ~60 `GPU*`, the whole `XR*`, `USB*`, `MIDI*`, `Presentation*` and `Sensor`
//! families, `CacheStorage`, `SubtleCrypto`, `ServiceWorker*`, `PublicKeyCredential`,
//! `WebTransport*` — plus `caches`, `cookieStore` and four `on*` handler rows of the `[Global]`
//! `Window` table. They are published from three different kinds of place and they now pass through
//! ONE gate, each carrying its own condition: `install::InterfaceDef::exposure` (generated from the
//! measurement, or the `secure` word in a hand table), `install::MemberDef::exposure`, or an
//! `exposure::gate(..)` argument at a hand singleton.
//!
//! # WHY IT TAKES A NON-LOOPBACK HOSTNAME TO SEE THE DEFECT AT ALL
//!
//! `http://127.0.0.1/` is potentially trustworthy, so `isSecureContext` is TRUE there and every one
//! of these names is CORRECTLY present — on this engine and on Chrome alike. FP5's own fixtures ran
//! on `about:blank` and on 127.0.0.1, which is why they could not have caught it, and it is why the
//! insecure half of this file uses a hostname that resolves to loopback without being `localhost`.
//!
//! # THE EXPECTATION IS A READING, AND IT IS READ FROM ONE PLACE
//!
//! `tools/sync/data/secure_context.json` is the measurement: every own name an insecure origin takes
//! off the window, read 2026-09-19 off Chrome 150.0.7871.46, Brave 151.1.93.138, Edge 151.0.4129.93
//! and Opera 135.0.5973.133 — headed under Xvfb over CDP, `navigator.webdriver` false in all eight
//! runs — from ONE `python3 -m http.server` at TWO hostnames, so the only difference between the runs
//! is trustworthiness:
//!
//! ```text
//! secure     http://127.0.0.1:<p>/page.html          isSecureContext true
//! insecure   http://web-platform.test:<p>/page.html  isSecureContext false
//! ```
//!
//! 248 names, 230 of them common to all four browsers, **0 gained** on the insecure origin, and the
//! four browsers never once disagree about a name they both have. That is what makes the condition a
//! PLATFORM fact rather than a bundle field, and it is why the same file feeds `gen_shells.py`: a
//! second copy of a measurement is a second definition, and the two would disagree. What this file
//! asserts is therefore the PLUMBING — that the engine removes exactly the measured names and
//! exactly the ones it publishes — not a restatement of the values.
//!
//! **The measurement contradicts the IDL, in both directions, for 53 names**, which is the whole
//! reason it is a measurement: webref marks `AudioWorkletNode`, `GeolocationPosition`, `PushManager`,
//! `SpeechRecognition`, `Navigator` and `Window` `[SecureContext]` and all four browsers KEEP them,
//! while it is silent about the 26 `XR*` layer/hand/plane interfaces, `Worklet`, `FontData`,
//! `KeyboardLayoutMap`, `caches`, `cookieStore`, `queryLocalFonts`, the three file pickers and the
//! four `on*` handlers, which all four browsers REMOVE. FP5.7 found the same thing one name at a
//! time: `launchQueue` and `fence` are NOT `[SecureContext]` on these builds whatever their shape
//! suggests, and both are absent from the measurement for that reason.
//!
//! # What makes this a rule and not a second copy of the installer
//!
//! The engine's two readings are compared as SETS: the names an origin change removes must be
//! exactly the measured names this bundle publishes — no more (a gate that caught something it should
//! not) and no fewer (a name gated without a measurement behind it, or a gate quietly dropped). The
//! left-hand side is computed over the WHOLE window rather than over any list here, so a name
//! installed tomorrow with a condition and no measurement behind it lands on the left and not on the
//! right, and this goes red.

use std::collections::{BTreeMap, BTreeSet};

use engine::Engine;

/// THE MEASUREMENT, read from the one file that holds it (see the module docs). Maps each name to the
/// browsers that took it away — a name is expected to go under bundle `b` exactly when `b`'s own
/// browser removed it.
fn measurement() -> BTreeMap<String, BTreeSet<String>> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/sync/data/secure_context.json");
    let raw = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
    let v: serde_json::Value = serde_json::from_str(&raw).expect("secure_context.json is JSON");
    let names = v.get("names").and_then(|n| n.as_object()).expect("secure_context.json has `names`");
    let out: BTreeMap<String, BTreeSet<String>> = names
        .iter()
        .map(|(k, v)| {
            let bs = v.as_array().expect("each name maps to the browsers that removed it");
            (k.clone(), bs.iter().map(|b| b.as_str().unwrap().to_string()).collect())
        })
        .collect();
    assert!(
        out.len() > 200,
        "the measurement is the 248 names an insecure origin removes; {} is not it",
        out.len()
    );
    out
}

/// The names this engine does not install AT ALL, so an origin cannot remove them — each one a
/// MISSING NAME of another lane's, measured on the browser and absent here, not a gate that fired.
///
/// They are listed so they are ASSERTED ABSENT rather than merely missing (the rule FP5 argued: an
/// absent name costs a fingerprint point, a stub costs the page). The list shrinking is progress and
/// turns this file red until it is shortened; a name APPEARING here without a reason is the failure
/// this guard exists for. Measured 2026-09-19, korvus at a secure origin vs each bundle's browser.
///
/// * `braveEthereum`, `braveSolana`, `cardano`, `solana` — Brave's four injected wallet globals,
///   absent from this engine under every bundle. They are not a Web API gap: they belong to the
///   PERSONA and land through `spec::Persona::window_namespaces`.
///
/// **`documentPictureInPicture` LEFT THIS LIST** (`core/html/document_pip.rs`). FP5.9
/// recorded it here as the one name that made this engine's removed-count one short of the browser's
/// under three bundles, for the stated reason that a name it does not install is a name it cannot
/// take away. It installs it now — `.window` is `null` and `requestWindow()` rejects with the
/// device's own `NotAllowedError: Document PiP requires user activation` — so the gate applies to it
/// and the shortfall is four.
const NOT_IMPLEMENTED: [&str; 4] = ["braveEthereum", "braveSolana", "cardano", "solana"];

/// A potentially-trustworthy origin and two that are not. The second insecure one is there so the
/// assertion cannot be read as being about one hostname: what decides is trustworthiness, and
/// `korvus-fp57.example` is a reserved name that resolves nowhere.
const SECURE_URL: &str = "http://127.0.0.1:8991/page.html";
const INSECURE_URLS: [&str; 2] =
    ["http://web-platform.test:8991/page.html", "http://korvus-fp57.example/page.html"];

const DOC: &str = "<!doctype html><html><head><title>fp57</title></head><body>probe</body></html>";

/// Every bundle directory that carries a manifest, sorted. The denominator is the tree, not a list.
fn bundles() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/korvus-persona/bundles");
    let mut out: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("manifest.toml").is_file())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no persona bundle in the tree");
    out
}

/// The browser a bundle id names, as the measurement spells it: `chrome-152-linux-20261016T1022` ->
/// `chrome-152`. Derived from the id rather than tabled, so a re-capture of the same browser on a new
/// date needs no edit here.
fn browser_of(bundle_id: &str) -> String {
    bundle_id.split('-').take(2).collect::<Vec<_>>().join("-")
}

/// The window own-globals the bundle's own capture records — read through the persona, which is the
/// object the engine reads it through, so a test cannot grade against a bundle the engine did not wear.
fn captured_globals(id: Option<&str>) -> BTreeSet<String> {
    use spec::Persona;
    let p = persona_for(id);
    let s = p
        .surface(spec::SurfaceRealm::Window)
        .unwrap_or_else(|| panic!("bundle {id:?} records no window surface"));
    s.globals.iter().cloned().collect()
}

/// The persona for a bundle id, or the SHIPPED default when there is none. One helper, so the
/// per-bundle loop and the default-persona row cannot select a persona two different ways.
fn persona_for(id: Option<&str>) -> persona::CapturedPersona {
    match id {
        Some(id) => persona::CapturedPersona::select(Some(id)).unwrap_or_else(|e| panic!("select bundle {id}: {e}")),
        None => persona::CapturedPersona::default(),
    }
}

/// What one bundle's engine reports at `url`: `isSecureContext`, and every own global name.
async fn realm_at(id: Option<&str>, url: &str) -> (bool, BTreeSet<String>) {
    let mut e = Engine::new(persona::worn_on_host(persona_for(id)));
    e.run().await.unwrap();
    // `load_page`, not `load_html`: the URL is the whole point, and `load_html` leaves `HostLocation`
    // at its `about:blank` default -- which is potentially trustworthy, and therefore cannot tell the
    // two halves of this test apart. The document has no subresource, so nothing is fetched and the
    // hostname is never resolved.
    e.load_page(DOC, url, &[]).await.unwrap();
    e.run().await.unwrap();
    e.eval(
        "fp57",
        r#"print("SECURE:" + window.isSecureContext);
           print("GLOBALS:" + JSON.stringify(Object.getOwnPropertyNames(window)));"#
            .to_string(),
    )
    .unwrap();
    e.run().await.unwrap();
    let out = e.take_output();
    let find = |k: &str| {
        out.iter()
            .find_map(|l| l.strip_prefix(k))
            .unwrap_or_else(|| panic!("no `{k}` line for {id:?} at {url}: {out:?}"))
            .to_string()
    };
    let secure = find("SECURE:") == "true";
    let names: Vec<String> = serde_json::from_str(&find("GLOBALS:")).expect("GLOBALS is JSON");
    (secure, names.into_iter().collect())
}

/// THE RULE, under every bundle: the names an insecure origin takes away are EXACTLY the measured
/// `[SecureContext]` names that bundle publishes.
///
/// Stated as a set equality in both directions on purpose. `⊇` alone would pass a gate that fired on
/// a name no browser gates; `⊆` alone would pass a gate quietly removed. And because the left-hand
/// side is computed over the WHOLE window rather than over a list in this file, a name installed
/// tomorrow with a condition and no measurement behind it lands on the left and not on the right, and
/// this goes red.
#[tokio::test]
async fn an_insecure_origin_removes_exactly_the_secure_context_names() {
    let measured = measurement();
    for id in bundles() {
        let browser = browser_of(&id);
        let (secure, on_secure) = realm_at(Some(&id), SECURE_URL).await;
        assert!(secure, "{id}: {SECURE_URL} must be a secure context -- loopback is potentially trustworthy");

        // The expectation: every measured [SecureContext] name this engine actually publishes on the
        // secure origin. Which names this bundle HAS is read from `on_secure`, not from the
        // measurement's per-browser `browsers` column: that column records which of the four stock
        // browsers EXPOSED a name at all, and presence is OS-dependent where gating is not -- Windows
        // Chrome exposes `EyeDropper` and this Linux Chrome does not, yet both gate it the same (the
        // measurement's own note: the browsers never disagree about a name they both have). A name's
        // being `[SecureContext]` is one platform fact, so membership in `measured` is the whole test;
        // intersecting with what is published is not a weakening -- a name that is not there cannot be
        // taken away, and names this engine does not publish are asserted absent by
        // `a_secure_origin_has_every_name_its_capture_records`.
        let expected: BTreeSet<String> = measured
            .keys()
            .filter(|n| on_secure.contains(*n))
            .cloned()
            .collect();
        assert!(
            expected.len() > 200,
            "{id} ({browser}): the gated set is ~230 names, not {} -- the measurement and this bundle's \
             published names barely intersect",
            expected.len()
        );

        for url in INSECURE_URLS {
            let (insecure_says_secure, on_insecure) = realm_at(Some(&id), url).await;
            assert!(!insecure_says_secure, "{id}: {url} must NOT be a secure context");

            let removed: BTreeSet<String> = on_secure.difference(&on_insecure).cloned().collect();
            let over: Vec<&String> = removed.difference(&expected).collect();
            let under: Vec<&String> = expected.difference(&removed).collect();
            assert!(
                over.is_empty() && under.is_empty(),
                "{id} at {url}: the names an insecure origin removes are not the measured ones this \
                 bundle publishes.\n  removed {} of {} expected\n  gated but the browser does not: {over:?}\n  \
                 the browser removes but this engine does not: {under:?}",
                removed.len(),
                expected.len()
            );
            // And nothing APPEARS. An insecure origin is a strict subset of a secure one; that is what
            // the same reading off all four browsers showed (0 gained, every one of them).
            let gained: BTreeSet<&String> = on_insecure.difference(&on_secure).collect();
            assert!(gained.is_empty(), "{id} at {url}: an insecure origin GAINED {gained:?}");
        }
    }
}

/// THE OTHER HALF OF THE RULE: on a SECURE origin every measured name is present exactly when the
/// worn capture records it and this engine implements it.
///
/// This is the half the gate above is blind to, and it is what stops the first test being made green
/// by removing a name from the secure origin too: a name dropped from BOTH origins is in neither the
/// removed set nor the expectation. A `[SecureContext]` name has to be PRESENT where the browser has
/// it, not merely absent where it does not.
#[tokio::test]
async fn a_secure_origin_has_every_name_its_capture_records_and_no_other() {
    let measured = measurement();
    for id in bundles() {
        let published = captured_globals(Some(&id));
        let (secure, on_secure) = realm_at(Some(&id), SECURE_URL).await;
        assert!(secure, "{id}: {SECURE_URL} must be a secure context");
        let mut absent = Vec::new();
        for name in measured.keys() {
            let want = published.contains(name) && !NOT_IMPLEMENTED.contains(&name.as_str());
            let got = on_secure.contains(name);
            if published.contains(name) && !got {
                absent.push(name.clone());
            }
            assert_eq!(
                got, want,
                "{id} at a SECURE origin: `{name}` present={got}, capture records it={}, this engine \
                 implements it={}",
                published.contains(name),
                !NOT_IMPLEMENTED.contains(&name.as_str())
            );
        }
        // The known shortfall, asserted by NAME so that it can only shrink: every measured name this
        // bundle's capture records but the engine does not install.
        let want: Vec<String> = NOT_IMPLEMENTED
            .iter()
            .filter(|n| published.contains(**n) && measured.contains_key(**n))
            .map(|n| n.to_string())
            .collect();
        assert_eq!(absent, want, "{id}: the measured names this engine does not install have changed");
    }
}

/// THE FIXTURE ITSELF, asserted rather than assumed: the WPT file the floor caught is one row of the
/// measurement, and it is spelled out here in its own words so that the connection between the rule
/// and the file cannot be lost.
///
/// ```js
/// test(() => { assert_false(window.hasOwnProperty('fetchLater')); },
///      `fetchLater() is not supported in non-secure context.`);
/// ```
///
/// And its converse, which that file does not assert and which an over-eager fix would break:
/// `fetchLater` IS there on a secure origin, and it still WORKS there.
#[tokio::test]
async fn fetch_later_is_the_wpt_row_and_both_directions_hold() {
    let published = captured_globals(None);
    assert!(published.contains("fetchLater"), "the shipped capture records fetchLater -- this test is about its CONDITION");
    assert!(measurement().contains_key("fetchLater"), "the measurement is what says fetchLater is conditional");

    for url in INSECURE_URLS {
        let (secure, names) = realm_at(None, url).await;
        assert!(!secure, "{url} is not a secure context");
        assert!(!names.contains("fetchLater"), "fetchLater() is not supported in non-secure context ({url})");
    }
    let (secure, names) = realm_at(None, SECURE_URL).await;
    assert!(secure);
    assert!(names.contains("fetchLater"), "fetchLater IS supported in a secure context ({SECURE_URL})");
}
