//! LIVE one-table gate for `Navigator` / `WorkerNavigator` (Phase 3 Track B; docs/PHASE-3-MIGRATION-PLAN.md 1).
//!
//! Boots the real engine -- whose context creation now builds the window `Navigator` prototype from
//! its `interface!` table (`webapi::install::live::install_window_navigator`) and points the running
//! `navigator` at it -- and asserts the RUNNING navigator is byte-identical (member names, order,
//! descriptor shape) AND value-identical (persona scalars, the intricate objects, the recording
//! answers) to the captured Brave surface. This is the proof the table + the native scalar getters +
//! the nav-glue bodies reproduce the old `dom/09g-navigator.js` layer exactly -- the precondition for
//! deleting it. The worker subset (`WorkerNavigator`, 23 members) is proven byte-identical by
//! `tests/navigator_shape.rs`; this file adds a live spot-check that the selector is that subset.

use engine::Engine;
use spec::Persona;

fn artifact(role: &str) -> String {
    std::fs::read_to_string(persona::shipped_artifact(role)).unwrap()
}

/// The ordered `name:kind:...` describe lines the installer must produce for an interface's own
/// prototype, from the surface fixture, plus the trailing non-enumerable `constructor`.
fn expected_proto(iface: &str) -> Vec<String> {
    let fx: serde_json::Value = serde_json::from_str(&artifact("surface")).unwrap();
    let mut out: Vec<String> = fx["interfaces"][iface]["members"].as_array().unwrap().iter().map(|m| {
        let n = m["name"].as_str().unwrap();
        match m["kind"].as_str().unwrap() {
            "accessor" => format!("{n}:accessor:e=1:c=1:s={}", m["set"].as_bool().unwrap_or(false) as u8),
            "method" => format!("{n}:data:e=1:c=1:w=1"),
            k => panic!("unexpected kind {k}"),
        }
    }).collect();
    // `constructor` AT THE CAPTURE'S SEAT, not appended. It is the last member
    // installed at interface-definition time, so a member a PARTIAL interface adds lands AFTER it --
    // `Navigator` 32 of 66, `Document` 240 of 247, `Screen` 9 of 11 with `onchange` and `isExtended`
    // behind it. Appending it was right on 83 % of the surface and wrong on exactly the interfaces a
    // fingerprinting script reads, and the index has been in the bundle since `3a5f0b60`.
    let at = fx["interfaces"][iface]["ctorIndex"].as_u64().map(|n| n as usize).unwrap_or(out.len()).min(out.len());
    out.insert(at, "constructor:data:e=0:c=1:w=1".to_string());
    out
}

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e
}

/// BYTE-IDENTICAL: the running navigator's prototype is the capture's 66-member window surface, member
/// for member, in order, with the WebIDL descriptor shape -- built entirely by the table + installer.
#[tokio::test]
async fn live_window_navigator_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    let describe = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
        const d = Object.getOwnPropertyDescriptor(o, n);
        return (d.get || d.set)
            ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
            : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
    }).join("\n");"#;
    let src = format!(r#"(() => {{ {describe} return desc(Object.getPrototypeOf(navigator)); }})()"#);
    let got = e.eval_value("nav-desc", src).unwrap().as_str().unwrap().to_string();
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        expected_proto("Navigator"),
        "the running navigator's prototype != the captured surface (table/installer must reproduce it)",
    );
}

/// VALUE-IDENTICAL: the persona scalars are the bundle's (native `[native code]` getters), the intricate
/// members answer as the recording says, the instance is stripped bare, and the interface object is a
/// constructor-less `[native code]` throw -- everything `09g-navigator.js` produced, now from the table.
#[tokio::test]
async fn live_window_navigator_is_value_identical_to_the_capture() {
    let mut e = page().await;
    let p = persona::CapturedPersona::default();
    let nav = p.navigator();
    let x = p.navigator_extra();
    let plugin_len = p.client_hints().plugins.len();

    let src = r#"(() => {
        let newThrew = false;
        try { new Navigator(); } catch (e) { newThrew = e instanceof TypeError; }
        const uaGet = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(navigator), "userAgent").get;
        return [
            "ua=" + navigator.userAgent,
            "plat=" + navigator.platform,
            "hc=" + navigator.hardwareConcurrency,
            "ownNames=" + JSON.stringify(Object.getOwnPropertyNames(navigator)),
            "ctor=" + navigator.constructor.name,
            "instanceof=" + (navigator instanceof Navigator),
            "newThrew=" + newThrew,
            "uaNative=" + uaGet.toString().includes("[native code]"),
            "gpc=" + navigator.globalPrivacyControl,
            "wd=" + navigator.webdriver,
            "dnt=" + navigator.doNotTrack,
            "keyboard=" + navigator.keyboard,
            "java=" + navigator.javaEnabled(),
            "gamepads=" + navigator.getGamepads().length + "/" + navigator.getGamepads().every(g => g === null),
            "plugins=" + navigator.plugins.length + "/" + (typeof navigator.plugins.item),
            "pdf=" + navigator.pdfViewerEnabled,
            "usbTag=" + Object.prototype.toString.call(navigator.usb),
            "usbSingleton=" + (navigator.usb === navigator.usb),
            "brave=" + (typeof navigator.brave),
            "uaData=" + navigator.userAgentData.platform,
            "vendor=" + navigator.vendor,
            "swTag=" + Object.prototype.toString.call(navigator.serviceWorker),
        ].join("|");
    })()"#;
    let got = e.eval_value("nav-val", src.to_string()).unwrap().as_str().unwrap().to_string();
    let fields: std::collections::HashMap<&str, &str> = got
        .split('|')
        .filter_map(|kv| kv.split_once('='))
        .collect();

    assert_eq!(fields["ua"], nav.user_agent, "userAgent");
    assert_eq!(fields["plat"], nav.platform, "platform");
    assert_eq!(fields["hc"], nav.hardware_concurrency.to_string(), "hardwareConcurrency");
    // The instance is stripped bare: EVERY member is on `Navigator.prototype`, which is where WebIDL
    // declares them. Any own property here would be a member that leaked back onto the instance.
    //
    // This used to assert `["plugins","mimeTypes"]`, because `dom/09i-singletons.js` wrote its real
    // PluginArray/MimeTypeArray as own accessors of the singleton. Asked directly, headless Chrome
    // 150.0.7871.46 on this machine answers `Object.prototype.hasOwnProperty.call(navigator,
    // "plugins")` FALSE (probed 2026-09-11 over a loopback server, with `getOwnPropertyNames
    // (navigator)` empty), so the engine was the one that was wrong and the expectation moved with the
    // fix: both rows are on the prototype (core/html/plugins.rs).
    assert_eq!(
        fields["ownNames"], "[]",
        "navigator owns nothing -- every member is on Navigator.prototype, as Chrome has it",
    );
    assert_eq!(fields["ctor"], "Navigator", "constructor.name");
    assert_eq!(fields["instanceof"], "true", "navigator instanceof Navigator");
    assert_eq!(fields["newThrew"], "true", "new Navigator() must throw TypeError");
    assert_eq!(fields["uaNative"], "true", "userAgent getter must be [native code]");
    // Chrome 150 has neither `globalPrivacyControl` nor `brave` (Brave 151, the persona until 2026-10-16:
    // `true`, and a `navigator.brave.isBrave` function).
    assert_eq!(fields["gpc"], "undefined", "globalPrivacyControl (not a Chrome 150 member)");
    assert_eq!(fields["wd"], "false", "webdriver");
    assert_eq!(fields["dnt"], "null", "doNotTrack (recording)");
    // Chrome 150 answers a Keyboard in a secure context (measured 2026-10-16, `--dump-dom` of a file:// page:
    // `[object Keyboard]`); Brave 151, the persona until then, answered `null`.
    assert_eq!(fields["keyboard"], "[object Keyboard]", "keyboard");
    assert_eq!(fields["java"], "false", "javaEnabled()");
    assert_eq!(fields["gamepads"], "4/true", "getGamepads() -> four nulls");
    assert_eq!(fields["plugins"], format!("{plugin_len}/function"), "plugins length + item()");
    assert_eq!(fields["pdf"], x.pdf_viewer_enabled.to_string(), "pdfViewerEnabled");
    assert_eq!(fields["usbTag"], "[object USB]", "navigator.usb is a branded USB instance");
    assert_eq!(fields["usbSingleton"], "true", "navigator.usb must be a per-navigator singleton");
    assert_eq!(fields["brave"], "undefined", "navigator.brave (not a Chrome 150 member)");
    assert_eq!(fields["uaData"], p.client_hints().platform, "userAgentData.platform");
    assert_eq!(fields["vendor"], x.vendor, "vendor");
    assert_eq!(fields["swTag"], "[object ServiceWorkerContainer]", "serviceWorker is host.js's real container");
}

/// EVERY bundle's worker `WorkerNavigator` member set is a strict subset of its own window
/// `Navigator`'s, and the table has a row for every one of them.
///
/// It used to compare two hand-written statics (`WORKER_NAVIGATOR_MEMBERS` against the fixture, and
/// then against `WINDOW_NAVIGATOR_MEMBERS`) -- a test that the copy had not drifted from the
/// recording. Both copies are deleted: the installer reads each realm's list off the ACTIVE
/// capture, so the drift this guarded against cannot happen, and the question worth asking instead is
/// the one the deleted statics quietly assumed -- that one table really can serve both realms of
/// every device in the tree.
#[test]
fn every_bundles_worker_navigator_is_a_subset_of_its_own_window_navigator() {
    use spec::{Persona, SurfaceRealm};
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/korvus-persona/bundles");
    let mut ids: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("manifest.toml").is_file())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    ids.sort();
    assert!(!ids.is_empty(), "no persona bundle in the tree");
    for id in ids {
        let p = persona::CapturedPersona::select(Some(&id)).unwrap_or_else(|e| panic!("select {id}: {e}"));
        let win = p.surface(SurfaceRealm::Window).unwrap();
        let wk = p.surface(SurfaceRealm::Worker).unwrap();
        let window = &win.members_of("Navigator").unwrap_or_else(|| panic!("{id}: no window Navigator")).members;
        let worker = &wk.members_of("WorkerNavigator").unwrap_or_else(|| panic!("{id}: no WorkerNavigator")).members;
        assert!(!worker.is_empty() && !window.is_empty(), "{id}: an empty member list compares perfectly");
        for m in worker {
            assert!(window.contains(m), "{id}: worker member {m} is not a window member");
            assert!(
                webapi::install::live::NAVIGATOR.member_on(m, false).is_some(),
                "{id}: the capture records WorkerNavigator.{m} and the table has no row for it"
            );
        }
        for m in window {
            assert!(
                webapi::install::live::NAVIGATOR.member_on(m, false).is_some(),
                "{id}: the capture records Navigator.{m} and the table has no row for it"
            );
        }
    }
}
