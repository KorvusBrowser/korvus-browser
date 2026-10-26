//! THE SURFACE FOLLOWS THE BUNDLE — every bundle in the tree, both realms.
//!
//! # What this asserts, and why it is a loop over a directory
//!
//! The engine's tables carry the UNION of the Chromium family's rows and the ACTIVE bundle's capture
//! selects from it. That claim has exactly two halves, and this file asserts both **for every bundle
//! `plugins/korvus-persona/bundles/` contains** — a loop, not a list, so dropping a third capture in
//! puts it under gate without anyone editing a test (derived, never counted).
//!
//! 1. **Presence and order.** For every interface the active capture records, the members the engine
//!    mounts on its prototype are a SUBSET of that capture's, in that capture's RELATIVE ORDER. Subset
//!    rather than equality because the union can lack a row for a member a capture records — that is
//!    an implementation gap, it is reported by name below, and it is bounded by a ratchet rather than
//!    waved through. A member the capture does NOT record must not be mounted at all; that direction
//!    is asserted exactly, because it is the direction a page reads as a tell.
//!
//! 2. **The differential.** Two bundles must not produce one surface. Before L7b the engine's whole
//!    surface hashed identically under both (`sha256 175d74ef…`, recorded)
//! because it was compiled from one capture, and
//!    the four observables that differ between the CAPTURES did not differ between the ENGINES. So
//!    the digests are asserted DIFFERENT, and `navigator.brave` — present in Brave's capture, absent
//!    from Chrome's — is asserted to follow its own capture in each.
//!
//! # Why it selects the persona explicitly instead of setting `KORVUS_BUNDLE`
//!
//! `std::env::set_var` is process-global and this binary runs its tests in parallel: a test that sets
//! the variable races every other test in the file, and the failure mode is a realm built from one
//! bundle graded against another. `CapturedPersona::select(Some(id))` is the same code path
//! `KORVUS_BUNDLE` takes (`selected()` is `select(env)`), with the id passed in — which is why that
//! split exists.

use std::collections::{BTreeMap, BTreeSet};

use engine::Engine;

/// Every bundle directory that carries a manifest, sorted. The denominator is the tree, not a list.
fn bundles() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/korvus-persona/bundles");
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

fn persona_for(id: &str) -> persona::CapturedPersona {
    persona::CapturedPersona::select(Some(id)).unwrap_or_else(|e| panic!("select bundle {id}: {e}"))
}

/// A bundle's WORKER capture reduced to its surface shape -- globals and each interface's member names
/// -- so two captures that describe the same worker surface compare equal. The worker surface is a
/// function of the browser engine, not the host OS (Web Share and Web Bluetooth are window-only), so
/// two desktop OSes of one Chrome share it; this is what lets the differential below tell a legitimate
/// shared surface from the engine collapsing two DIFFERENT captures onto one.
fn worker_capture_shape(id: &str) -> String {
    let path = persona_for(id).artifact("worker-surface");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))).unwrap();
    let globals = v["globals"].to_string();
    let mut ifaces: Vec<String> = v["interfaces"]
        .as_object()
        .unwrap_or_else(|| panic!("{id}: worker-surface records no interfaces"))
        .iter()
        .map(|(k, iv)| {
            let members: Vec<&str> =
                iv["members"].as_array().map(|a| a.iter().filter_map(|m| m["name"].as_str()).collect()).unwrap_or_default();
            format!("{k}:{}", members.join(","))
        })
        .collect();
    ifaces.sort();
    format!("{globals}|{}", ifaces.join(";"))
}

/// The capture's own answer for one realm, read through the persona — the same object the engine
/// reads it through, so a test cannot grade against a bundle the engine did not wear.
fn capture(id: &str, realm: spec::SurfaceRealm) -> std::sync::Arc<spec::RealmSurface> {
    use spec::Persona;
    persona_for(id)
        .surface(realm)
        .unwrap_or_else(|| panic!("bundle {id} records no {:?} surface", realm))
}

/// Every own STRING key of every interface prototype and interface object on the live window global,
/// in the order a page reads them. One eval, because 900 round trips through `eval_value` is minutes.
const DUMP: &str = r#"JSON.stringify((() => {
  const out = {};
  for (const name of Object.getOwnPropertyNames(globalThis)) {
    let v; try { v = globalThis[name]; } catch (e) { continue; }
    if (typeof v !== "function") continue;
    let proto; try { proto = v.prototype; } catch (e) { continue; }
    const rec = { statics: Object.getOwnPropertyNames(v) };
    if (proto && (typeof proto === "object" || typeof proto === "function")) {
      rec.members = Object.getOwnPropertyNames(proto);
    } else {
      rec.members = [];
    }
    out[name] = rec;
  }
  return { globals: Object.getOwnPropertyNames(globalThis), interfaces: out };
})())"#;

struct Live {
    globals: Vec<String>,
    interfaces: BTreeMap<String, LiveIface>,
    brave_present: bool,
    /// A nested browsing context's window, read the same way -- its own global keys in
    /// order, and `navigator.brave` read THROUGH it. Empty for the worker realm, which has no frames.
    frame_globals: Vec<String>,
    frame_brave_present: bool,
}

/// The same own-key read, off the `contentWindow` of an `<iframe>` appended to an idle page, beside
/// the page's own list read in the same eval. Index keys are the WindowProxy's frame slots
/// (`window[0]`), not globals, and are dropped on both sides.
const FRAME_DUMP: &str = r#"JSON.stringify((() => {
  const f = document.createElement("iframe");
  document.body.appendChild(f);
  const w = f.contentWindow;
  const own = (o) => Object.getOwnPropertyNames(o).filter((n) => !/^[0-9]+$/.test(n));
  return { top: own(globalThis), frame: own(w), brave: typeof w.navigator.brave };
})())"#;

#[derive(serde::Deserialize)]
struct LiveIface {
    members: Vec<String>,
    statics: Vec<String>,
}

#[derive(serde::Deserialize)]
struct RawDump {
    globals: Vec<String>,
    interfaces: BTreeMap<String, LiveIface>,
}

async fn live_window(id: &str) -> Live {
    let mut e = Engine::new(persona::worn_on_host(persona_for(id)));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><head><title>t</title></head><body>x</body></html>").unwrap();
    e.run().await.unwrap();
    let raw = e.eval_value("persona-surface-dump", DUMP.into()).unwrap();
    let dump: RawDump =
        serde_json::from_str(raw.as_str().expect("the probe returns a JSON string")).expect("the dump parses");
    let brave = e.eval_value("persona-surface-brave", "typeof navigator.brave".into()).unwrap();
    let frame = e.eval_value("persona-surface-frame", FRAME_DUMP.into()).unwrap();
    let frame: serde_json::Value =
        serde_json::from_str(frame.as_str().expect("the frame probe returns a JSON string")).expect("the frame dump parses");
    let strs = |v: &serde_json::Value| -> Vec<String> {
        v.as_array().expect("an array of names").iter().map(|n| n.as_str().unwrap_or_default().to_string()).collect()
    };
    let top_now: Vec<String> = strs(&frame["top"]);
    let top_before: Vec<String> = dump.globals.iter().filter(|n| !n.bytes().all(|b| b.is_ascii_digit())).cloned().collect();
    assert_eq!(top_now, top_before, "{id}: the page's own globals changed between two reads of an idle page");
    Live {
        globals: dump.globals,
        interfaces: dump.interfaces,
        brave_present: brave.as_str() == Some("object"),
        frame_globals: strs(&frame["frame"]),
        frame_brave_present: frame["brave"].as_str() == Some("object"),
    }
}

/// Is `sub` a subsequence of `full` — i.e. does it appear in `full`'s relative order?
fn is_subsequence(sub: &[&str], full: &[String]) -> Option<String> {
    let mut it = full.iter();
    for want in sub {
        if !it.any(|have| have == want) {
            return Some((*want).to_string());
        }
    }
    None
}

/// A stable digest of the whole live surface: the global's own keys in order, then every interface's
/// own prototype and interface-object keys in order. Exactly the thing that hashed identically under
/// both bundles before this unit.
fn digest(live: &Live) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "G {}", live.globals.join(","));
    for (name, i) in &live.interfaces {
        let _ = writeln!(s, "P {name} {}", i.members.join(","));
        let _ = writeln!(s, "S {name} {}", i.statics.join(","));
    }
    s
}

/// Members the active capture records that no row in the union provides. A CEILING measured at the
/// commit that introduced it; it may fall and must never rise, and every name is printed above so the
/// unit that closes them can see which they are.
///
/// **33 at L7b.1, and every one of them is Chrome's**: the generated registry is still emitted from
/// the SHIPPED capture alone, so Protected Audience's thirteen `Navigator` members, Topics, Shared
/// Storage, Attribution Reporting, Private State Tokens and FedCM have no row to select. L7b.2 makes
/// the registry the union over every bundle in the tree and takes this to 0 — the selector half of
/// the join landed first because it is green on its own, and because a union with nothing filtering it
/// would have put Chrome's interfaces on the Brave surface.
const GAP_CEILING: usize = 0;

/// Globals a capture records that this engine does not implement AT ALL. A CEILING over every bundle,
/// not a per-bundle list; it may fall and must never rise.
///
/// 46 at L7c. **A later change took it to 10** by minting 33 of them --- `external`, `styleMedia`,
/// `clientInformation`, `offscreenBuffering`, `find`, `fetchLater`, `launchQueue`, `sharedStorage`,
/// `fence`, the twelve webkit aliases, the five WebGPU constant namespaces and the five
/// user-activation-gated calls, plus the six `BarProp`s and `viewport` off two tables of their own.
/// 0 extra under every bundle, both realms, and the order ratchet stayed at 0.
///
/// WHAT THE REMAINING 5 ARE, because "missing" says nothing on its own:
///
/// * **1** (`navigation`) needs a BODY on a member of an interface the generated registry already
///   installs, and `tests/install_ledger.rs` refuses a second writer of a slot the installer mounted
///   (I2). The route is a table per interface, the way `core/feature_policy/feature_policy.rs` and
///   FP5's own `core/html/bar_props.rs` went; `Navigation` carries event-handler rows besides, and a
///   real `currentEntry`/`entries()` means real session-history state rather than a list, which is
///   why it is a lane of its own (FP5.8). Installing it WITHOUT its bodies would be worse than
///   leaving it out: a page that finds a name and calls through it throws on a stub, where a page on
///   an engine that lacks the name takes its fallback and works.
///
///   **A later change took five off this list** and the ceiling fell 10 -> 9 -> 7 -> 6 -> 5 with them:
///   `visualViewport` (`core/html/visual_viewport.rs` -- FP13 measured that the INTERFACE was on the
///   global while the INSTANCE was not, so a bare reference threw `ReferenceError` on `zed.dev`),
///   `crashReport` (`core/html/crash_report.rs` -- a real byte budget, so `set()` answers
///   `InvalidStateError`, `NotAllowedError` or nothing at all, as the buffer decides),
///   `documentPictureInPicture` (`core/html/document_pip.rs` -- `.window` is `null`, not a stub's
///   `undefined`) and `trustedTypes` (`core/trusted_types/` -- five tables, because a policy has to
///   round-trip: `createPolicy` -> `createHTML` -> `isHTML` true, which is the exact call FP5 named
///   as the one a stub breaks) and `scheduler` (`core/html/scheduler.rs` -- a real priority queue on
///   this realm's own macrotask hook, so `postTask` RUNS the task and resolves with its value).
/// * **4**, under `brave-151` only, are Brave's wallet objects (`braveEthereum`, `braveSolana`,
///   `solana`, `cardano`). They are not a Web API gap: they are objects Brave's own wallet injects,
///   they belong to the PERSONA and not to `core/`, and they land through
///   `spec::Persona::window_namespaces` --- the seam `opr`/`g_opr` already use.
const MISSING_GLOBALS_CEILING: usize = 5;

#[tokio::test]
async fn every_bundle_gets_its_own_surface_and_only_its_own() {
    let mut digests: BTreeMap<String, String> = BTreeMap::new();
    let mut gaps_total = 0usize;
    for id in bundles() {
        let live = live_window(&id).await;
        let cap = capture(&id, spec::SurfaceRealm::Window);

        // NON-VACUITY FIRST: an empty comparison scores perfectly, and this file would then be a
        // green light that looked at nothing.
        assert!(
            live.interfaces.len() > 500,
            "{id}: only {} interface objects on the live global -- the dump is not working",
            live.interfaces.len()
        );

        let mut extra: Vec<String> = Vec::new();
        let mut gaps: Vec<String> = Vec::new();
        let mut out_of_order: Vec<String> = Vec::new();
        let mut misseated: Vec<String> = Vec::new();
        let mut compared = 0usize;
        let mut seated = 0usize;
        let mut seated_inside = 0usize;
        for (name, li) in &live.interfaces {
            let Some(want) = cap.members_of(name) else { continue };
            compared += 1;
            // WHERE `constructor` SITS, on the live global, wearing this bundle.
            //
            // `Object.getOwnPropertyNames(X.prototype).indexOf("constructor")` is one read a page gets
            // for free while walking a prototype, and it was 0 on 635 of brave-151's 930 interfaces
            // where every browser in the family puts it last. The rule the captures record is NOT
            // "last": `constructor` is the last member installed at INTERFACE-DEFINITION time, so a
            // PARTIAL interface's members land after it — `Element` 152 of 152, `Navigator` 32 of 66,
            // `Screen` 9 of 11 with `onchange` and `isExtended` behind it. The expected number is the
            // worn bundle's `ctorIndex`, counted in the keys this engine actually mounts, so an
            // interface with a missing row is short by that row rather than failing here for it.
            if let Some(ci) = want.ctor_index {
                seated += 1;
                if ci > 0 && ci < want.members.len() {
                    seated_inside += 1;
                }
                let before: BTreeSet<&str> = want.members.iter().take(ci).map(String::as_str).collect();
                let at = li.members.iter().position(|m| m == "constructor");
                let expect = li.members.iter().filter(|m| before.contains(m.as_str())).count();
                if at != Some(expect) {
                    misseated.push(format!(
                        "{name}: constructor at {at:?}, this capture seats it at {expect} (ctorIndex {ci} of {} recorded members)",
                        want.members.len()
                    ));
                }
            }
            let recorded: BTreeSet<&str> = want.members.iter().map(String::as_str).collect();
            // `constructor` is WebIDL's own and every capture records it implicitly by not listing it;
            // a symbol key is not in `Object.getOwnPropertyNames` at all, so it cannot appear here.
            let mounted: Vec<&str> =
                li.members.iter().map(String::as_str).filter(|m| *m != "constructor").collect();
            for m in &mounted {
                if !recorded.contains(m) {
                    extra.push(format!("{name}.{m}"));
                }
            }
            let mounted_set: BTreeSet<&str> = mounted.iter().copied().collect();
            for m in &want.members {
                if !mounted_set.contains(m.as_str()) {
                    gaps.push(format!("{name}.{m}"));
                }
            }
            if let Some(first) = is_subsequence(&mounted, &want.members) {
                out_of_order.push(format!("{name}: {first} is out of the capture's order"));
            }
        }
        assert!(compared > 400, "{id}: only {compared} interfaces were compared against the capture");
        // NON-VACUITY FOR THE SEAT, BOTH HALVES, before it is asserted. An empty comparison passes,
        // and a comparison made only of interfaces whose `constructor` the capture puts LAST cannot
        // tell the rule from the constant — so the partial-extended family is required to be present.
        assert!(seated > 500, "{id}: only {seated} interfaces carry a recorded `ctorIndex`; the seat below is not being graded");
        assert!(
            seated_inside >= 20,
            "{id}: only {seated_inside} of {seated} interfaces have `constructor` strictly INSIDE their member list. \
             That family is what separates the rule (constructor is last at interface-definition time, partials land after it) \
             from the observation (constructor is last); with it empty a hard-coded constant would pass."
        );
        misseated.sort();
        eprintln!("[persona-surface] {id}: constructor seated as this capture has it on {} of {seated} interfaces ({seated_inside} with it strictly inside)", seated - misseated.len());
        assert!(
            misseated.is_empty(),
            "{id}: {} prototype(s) mount `constructor` where this capture does not -- one `indexOf` is the whole read:\n  {}",
            misseated.len(),
            misseated.join("\n  ")
        );
        // THE SIX FP5 MEASURED AND DELIBERATELY LEFT UNASSERTED, plus `Element` as the interface that
        // already agreed and must not move, and the partial family as the half a constant fails.
        // Named because they are exactly where the shell path and the device disagreed:
        // `VisualViewport` was 0 against 10, the widest single gap measured on this goal, so it is the
        // canary that the fix PLACES `constructor` rather than defaulting it to the end. Every
        // expected number is this bundle's own `ctorIndex`; not one of them is written down here.
        let mut witnesses: Vec<String> = Vec::new();
        for iface in [
            "BarProp", "External", "VisualViewport", "Scheduler", "Viewport", "CrashReportContext",
            "Element", "Navigator", "Performance", "StorageManager", "Screen",
        ] {
            let Some(want) = cap.members_of(iface) else { continue };
            let Some(li) = live.interfaces.get(iface) else {
                panic!("{id}: the capture records {iface} and the live global has no such interface object")
            };
            let ci = want.ctor_index.unwrap_or_else(|| panic!("{id}: the capture records no ctorIndex for {iface}"));
            let before: BTreeSet<&str> = want.members.iter().take(ci).map(String::as_str).collect();
            let at = li.members.iter().position(|m| m == "constructor");
            let expect = li.members.iter().filter(|m| before.contains(m.as_str())).count();
            assert_eq!(at, Some(expect), "{id}: {iface}: constructor at {at:?}, this capture seats it at {expect}");
            witnesses.push(format!("{iface} {expect}/{}", li.members.len() - 1));
        }
        assert!(witnesses.len() >= 10, "{id}: only {} of the named seat witnesses were checked: {witnesses:?}", witnesses.len());
        eprintln!("[persona-surface] {id}: seat witnesses {}", witnesses.join(" · "));

        extra.sort();
        out_of_order.sort();
        gaps.sort();
        eprintln!(
            "[persona-surface] {id}: {compared} interfaces compared, {} korvus-only members, \
             {} capture members with no row, {} prototypes out of order",
            extra.len(),
            gaps.len(),
            out_of_order.len()
        );
        for g in gaps.iter().take(40) {
            eprintln!("    no row: {g}");
        }
        assert!(
            extra.is_empty(),
            "{id}: the engine mounts {} member(s) this capture does not record -- a page reads every \
             one of them as a tell, and the bundle is the denominator:\n  {}",
            extra.len(),
            extra.join("\n  ")
        );
        assert!(
            out_of_order.is_empty(),
            "{id}: {} prototype(s) enumerate their members in an order this capture does not:\n  {}",
            out_of_order.len(),
            out_of_order.join("\n  ")
        );
        gaps_total += gaps.len();

        // THE GLOBAL ITSELF, both directions. A korvus-only global is a name `typeof` finds on a
        // device that does not have it, and it is the direction this unit had to fix twice: the DOM
        // spine's `HTML*Element` pass minted Chrome's `HTMLFencedFrameElement` on a Brave page (it
        // walks the registry's parent links, not the capture), and `install::live`'s domain installs
        // put Brave's `FontFaceSet` on a Chrome one. Asserted at zero in every bundle.
        let recorded: BTreeSet<&str> = cap.globals.iter().map(String::as_str).collect();
        let extra_globals: Vec<&str> =
            live.globals.iter().map(String::as_str).filter(|g| !recorded.contains(g)).collect();
        assert!(
            extra_globals.is_empty(),
            "{id}: the engine exposes {} global(s) this capture does not record: {extra_globals:?}",
            extra_globals.len()
        );
        let live_set: BTreeSet<&str> = live.globals.iter().map(String::as_str).collect();
        let missing_globals: Vec<&str> =
            cap.globals.iter().map(String::as_str).filter(|g| !live_set.contains(g)).collect();
        eprintln!("[persona-surface] {id}: {} capture globals this engine does not implement", missing_globals.len());
        assert!(
            missing_globals.len() <= MISSING_GLOBALS_CEILING,
            "{id}: {} capture global(s) missing (ceiling {MISSING_GLOBALS_CEILING}): {missing_globals:?}",
            missing_globals.len()
        );

        // `navigator.brave` follows the capture, in both directions. This is the observable
        // recorded as a leak: true under both bundles
        // where the captures say true and false.
        let recorded_brave = cap.members_of("Navigator").map(|m| m.members.iter().any(|x| x == "brave")).unwrap_or(false);
        assert_eq!(
            live.brave_present, recorded_brave,
            "{id}: navigator.brave is {} on the engine and {} in the capture",
            live.brave_present, recorded_brave
        );

        // A NESTED CONTEXT WEARS THE SAME PERSONA. Chrome runs one install for a frame's
        // window and the page's, so their own keys are one list in one order (measured on Chrome 150:
        // 1,224 and 1,224, no difference at any index). Here the frame is built from the page realm's
        // sealed install surface, so its list must be the page's exactly -- which puts every
        // assertion above on the frame too, under every bundle this loop visits.
        let top_globals: Vec<&str> =
            live.globals.iter().map(String::as_str).filter(|n| !n.bytes().all(|b| b.is_ascii_digit())).collect();
        let frame_globals: Vec<&str> = live.frame_globals.iter().map(String::as_str).collect();
        assert!(frame_globals.len() > 500, "{id}: the frame's window has only {} own globals", frame_globals.len());
        assert_eq!(
            frame_globals, top_globals,
            "{id}: a frame's window does not carry the page realm's global surface, in its order"
        );
        assert_eq!(
            live.frame_brave_present, recorded_brave,
            "{id}: navigator.brave through a frame's window is {} and the capture says {}",
            live.frame_brave_present, recorded_brave
        );

        digests.insert(id.clone(), digest(&live));
    }

    assert!(
        gaps_total <= GAP_CEILING,
        "{gaps_total} capture member(s) have no row in the union (ceiling {GAP_CEILING}). \
         Every one is printed above; the fix is a row, never a lowered ceiling."
    );

    // THE DIFFERENTIAL. Two devices, two surfaces.
    let ids: Vec<&String> = digests.keys().collect();
    for i in 0..ids.len() {
        for j in (i + 1)..ids.len() {
            assert_ne!(
                digests[ids[i]], digests[ids[j]],
                "{} and {} produce the SAME surface. That is the state P3 exists to end: the engine \
                 wearing one device and having another's interfaces.",
                ids[i], ids[j]
            );
        }
    }
}

/// The same claim in the WORKER realm, which is a different capture with a different answer -- 335
/// own globals against the window's 1,206, and one interface's difference between the two bundles.
///
/// `core/korvus-webapi/tests/worker_surface.rs` grades this realm far more thoroughly (descriptors,
/// nativeness, function names and lengths, the singletons), but against the bundle the PROCESS wears,
/// one per run. This is the loop: every bundle in the tree, in one process, so the per-bundle claim is
/// standing gate rather than a measurement somebody took once.
#[tokio::test]
async fn every_bundle_gets_its_own_worker_surface_and_only_its_own() {
    let mut digests: BTreeMap<String, String> = BTreeMap::new();
    for id in bundles() {
        let live = live_worker(&id).await;
        let cap = capture(&id, spec::SurfaceRealm::Worker);
        assert!(
            live.interfaces.len() > 200,
            "{id}: only {} interface objects in the worker global -- the dump is not working",
            live.interfaces.len()
        );
        let mut extra: Vec<String> = Vec::new();
        let mut gaps: Vec<String> = Vec::new();
        let mut out_of_order: Vec<String> = Vec::new();
        let mut misseated: Vec<String> = Vec::new();
        let mut compared = 0usize;
        let mut seated = 0usize;
        let mut seated_inside = 0usize;
        for (name, li) in &live.interfaces {
            let Some(want) = cap.members_of(name) else { continue };
            compared += 1;
            // WHERE `constructor` SITS IN THIS REALM, read off the WORKER capture.
            //
            // The window half above has asserted this since FP12; the worker realm could not, because
            // no `worker.surface.json` recorded `ctorIndex` and the engine borrowed the window's
            // index for the interfaces the two captures described identically (R22). The artifacts
            // were re-read from the four browsers, so this is the same assertion against this realm's
            // own measurement -- and the two realms do NOT always agree: `Performance` is 13 of 13
            // here and 13 of 18 in the window, because a worker's `Performance` is a different set of
            // partials. Nothing below is a literal; the expectation is this capture's own index,
            // counted in the keys the engine actually mounts.
            if let Some(ci) = want.ctor_index {
                seated += 1;
                if ci > 0 && ci < want.members.len() {
                    seated_inside += 1;
                }
                let before: BTreeSet<&str> = want.members.iter().take(ci).map(String::as_str).collect();
                let at = li.members.iter().position(|m| m == "constructor");
                let expect = li.members.iter().filter(|m| before.contains(m.as_str())).count();
                if at != Some(expect) {
                    misseated.push(format!(
                        "{name}: constructor at {at:?}, this capture seats it at {expect} (ctorIndex {ci} of {} recorded members)",
                        want.members.len()
                    ));
                }
            }
            let recorded: BTreeSet<&str> = want.members.iter().map(String::as_str).collect();
            let mounted: Vec<&str> =
                li.members.iter().map(String::as_str).filter(|m| *m != "constructor").collect();
            for m in &mounted {
                if !recorded.contains(m) {
                    extra.push(format!("{name}.{m}"));
                }
            }
            let mounted_set: BTreeSet<&str> = mounted.iter().copied().collect();
            for m in &want.members {
                if !mounted_set.contains(m.as_str()) {
                    gaps.push(format!("{name}.{m}"));
                }
            }
            if let Some(first) = is_subsequence(&mounted, &want.members) {
                out_of_order.push(format!("{name}: {first} is out of the capture's order"));
            }
        }
        assert!(compared > 200, "{id}: only {compared} worker interfaces were compared");
        // NON-VACUITY FOR THE SEAT, BOTH HALVES, exactly as the window half states them: an empty
        // comparison passes anything, and one made only of interfaces the capture puts `constructor`
        // LAST on cannot tell the rule from the constant.
        assert!(seated > 200, "{id} (worker): only {seated} interfaces carry a recorded `ctorIndex`; the seat below is not being graded");
        assert!(
            seated_inside >= 10,
            "{id} (worker): only {seated_inside} of {seated} interfaces have `constructor` strictly INSIDE their member list. \
             That family is what separates the rule (constructor is last at interface-definition time, partials land after it) \
             from the observation (constructor is last); with it empty a hard-coded constant would pass."
        );
        misseated.sort();
        eprintln!("[persona-surface/worker] {id}: constructor seated as this capture has it on {} of {seated} interfaces ({seated_inside} with it strictly inside)", seated - misseated.len());
        assert!(
            misseated.is_empty(),
            "{id} (worker): {} prototype(s) mount `constructor` where this capture does not:\n  {}",
            misseated.len(),
            misseated.join("\n  ")
        );
        // THE WITNESSES ARE THE INTERFACES A BORROW FROM THE WINDOW COULD NOT HAVE SEATED -- the
        // worker-only ones, and the ones whose two captures list different members, which is where
        // FP12's borrow declined and left the seat unset. They are the rows that say this realm is
        // read from its OWN artifact. Each is asserted to be unborrowable and to sit strictly inside
        // its member list; both facts are read off the two captures rather than written here.
        let win = capture(&id, spec::SurfaceRealm::Window);
        let mut witnesses: Vec<String> = Vec::new();
        for iface in ["WorkerNavigator", "WorkerGlobalScope", "StorageManager", "ServiceWorkerRegistration"] {
            let Some(want) = cap.members_of(iface) else {
                panic!("{id} (worker): the capture records no {iface}")
            };
            let ci = want.ctor_index.unwrap_or_else(|| panic!("{id} (worker): the capture records no ctorIndex for {iface}"));
            assert!(
                !win.members_of(iface).is_some_and(|w| w.members == want.members),
                "{id} (worker): {iface} is a witness because the window capture cannot supply its seat, and it now can -- \
                 pick a witness the two captures still describe differently"
            );
            assert!(
                ci > 0 && ci < want.members.len(),
                "{id} (worker): {iface}: this capture must put `constructor` strictly inside its {} members, and it is at {ci}",
                want.members.len()
            );
            let Some(li) = live.interfaces.get(iface) else {
                panic!("{id} (worker): the capture records {iface} and this realm has no such interface object")
            };
            let before: BTreeSet<&str> = want.members.iter().take(ci).map(String::as_str).collect();
            let at = li.members.iter().position(|m| m == "constructor");
            let expect = li.members.iter().filter(|m| before.contains(m.as_str())).count();
            assert_eq!(at, Some(expect), "{id} (worker): {iface}: constructor at {at:?}, this capture seats it at {expect}");
            witnesses.push(format!("{iface} {expect}/{}", li.members.len() - 1));
        }
        eprintln!("[persona-surface/worker] {id}: seat witnesses {}", witnesses.join(" · "));
        extra.sort();
        gaps.sort();
        out_of_order.sort();
        eprintln!(
            "[persona-surface/worker] {id}: {compared} interfaces compared, {} korvus-only members, \
             {} capture members with no row, {} prototypes out of order",
            extra.len(), gaps.len(), out_of_order.len()
        );
        assert!(extra.is_empty(), "{id} (worker): members this capture does not record:\n  {}", extra.join("\n  "));
        assert!(gaps.is_empty(), "{id} (worker): capture members with no row:\n  {}", gaps.join("\n  "));
        assert!(out_of_order.is_empty(), "{id} (worker): prototypes out of the capture's order:\n  {}", out_of_order.join("\n  "));

        let recorded: BTreeSet<&str> = cap.globals.iter().map(String::as_str).collect();
        let extra_globals: Vec<&str> =
            live.globals.iter().map(String::as_str).filter(|g| !recorded.contains(g)).collect();
        assert!(extra_globals.is_empty(), "{id} (worker): korvus-only globals: {extra_globals:?}");
        let live_set: BTreeSet<&str> = live.globals.iter().map(String::as_str).collect();
        let missing: Vec<&str> = cap.globals.iter().map(String::as_str).filter(|g| !live_set.contains(g)).collect();
        assert!(missing.is_empty(), "{id} (worker): capture globals this realm lacks: {missing:?}");

        digests.insert(id.clone(), digest(&live));
    }
    // Two bundles produce the same engine worker surface IFF their worker CAPTURES are the same shape.
    // Every desktop OS of one Chrome shares a worker surface -- the OS-specific members (Web Share, Web
    // Bluetooth) are window-only -- so `chrome-152-linux` and `chrome-152-windows` MAY match here. What
    // must never happen is the engine collapsing two DIFFERENT captures onto one surface (the bug this
    // lane was written for, `sha256 175d74ef…`), so the engine's sameness must mirror the captures',
    // pair for pair.
    let caps: BTreeMap<String, String> = digests.keys().map(|id| (id.clone(), worker_capture_shape(id))).collect();
    let ids: Vec<&String> = digests.keys().collect();
    for i in 0..ids.len() {
        for j in (i + 1)..ids.len() {
            let (a, b) = (ids[i], ids[j]);
            assert_eq!(
                digests[a] == digests[b],
                caps[a] == caps[b],
                "{a} and {b}: the engine worker surfaces {} but their captures {}",
                if digests[a] == digests[b] { "match" } else { "differ" },
                if caps[a] == caps[b] { "match" } else { "differ" },
            );
        }
    }
}

/// The worker realm's own keys, dumped from inside a real DOM worker and posted back -- the only way
/// to read a realm that lives on another thread. Same shape as the window's [`DUMP`], minus
/// `navigator.brave` (a worker has no `navigator.brave` in either capture).
const WORKER_DUMP: &str = r#"self.postMessage(JSON.stringify((() => {
  const out = {};
  for (const name of Object.getOwnPropertyNames(globalThis)) {
    let v; try { v = globalThis[name]; } catch (e) { continue; }
    if (typeof v !== "function") continue;
    let proto; try { proto = v.prototype; } catch (e) { continue; }
    const rec = { statics: Object.getOwnPropertyNames(v) };
    rec.members = (proto && (typeof proto === "object" || typeof proto === "function"))
      ? Object.getOwnPropertyNames(proto) : [];
    out[name] = rec;
  }
  return { globals: Object.getOwnPropertyNames(globalThis), interfaces: out };
})()));"#;

async fn live_worker(id: &str) -> Live {
    let mut e = Engine::new(persona::worn_on_host(persona_for(id)));
    e.run().await.unwrap();
    e.load_document("<!doctype html><html><head></head><body></body></html>").unwrap();
    e.run().await.unwrap();
    let reg = format!(
        "globalThis.__workerScripts = {{ './dump.js': {} }};",
        serde_json::to_string(WORKER_DUMP).unwrap()
    );
    e.eval("worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval("dump-driver", "const w = new Worker('./dump.js'); w.onmessage = (ev) => print('WS:' + ev.data);".into())
        .unwrap();
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    let json = loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(line) = out.iter().find(|l| l.starts_with("WS:")) {
            break line["WS:".len()..].to_string();
        }
        assert!(start.elapsed() <= std::time::Duration::from_secs(20), "{id}: the worker never posted its surface: {out:?}");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    let dump: RawDump = serde_json::from_str(&json).expect("the worker dump parses");
    Live { globals: dump.globals, interfaces: dump.interfaces, brave_present: false, frame_globals: Vec::new(), frame_brave_present: false }
}

