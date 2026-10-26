//! Byte-identical SURFACE-SHAPE gate for the one-table Navigator
//! (design §0.2). Installs the runtime `NAVIGATOR` table onto a throwaway prototype and asserts the
//! resulting own properties (names, ORDER, and descriptor shape) equal the captured surface, member
//! for member. This proves the table + installer reproduce Navigator's exact shape; it does NOT check
//! getter VALUES (those read HostPersona and need the live engine), which is why every body is still
//! `stub` here -- a stub attr and a real attr install the same descriptor.
//!
//! WHAT IT USED TO PASS IN, AND WHAT IT ASKS NOW. The window install was checked against
//! a selector this test built from the fixture, and the worker install against
//! `WORKER_NAVIGATOR_MEMBERS` -- a list in `core/navigator/navigator.rs` that this file asserted was
//! equal to the fixture's order. Both are gone: the member list of a realm is the ACTIVE bundle's, and
//! the installer reads it itself (`install::installer::join`). So the test SEEDS a named bundle's
//! surface and passes NO selector, which is a strictly stronger question -- it asks whether the join
//! reproduces the capture, where before it asked whether the installer could walk a list handed to it.
//!
//! And it asks it of EVERY bundle in the tree, both realms, because the claim P3 makes is about all
//! of them: the directory is the denominator.

use deno_core::{v8, RuntimeOptions};
use webapi::install::installer::{self, Flavor, InstallLedger};
use webapi::install::live::NAVIGATOR;
use webapi::install::verify::describe_own;

/// Every bundle directory that carries a manifest, sorted.
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

fn surface_of(id: &str, realm: spec::SurfaceRealm) -> std::sync::Arc<spec::RealmSurface> {
    use spec::Persona;
    persona::CapturedPersona::select(Some(id))
        .unwrap_or_else(|e| panic!("select bundle {id}: {e}"))
        .surface(realm)
        .unwrap_or_else(|| panic!("bundle {id} records no {realm:?} surface"))
}

/// Ordered `(name, kind)` for an interface's members from a bundle's surface file.
fn fixture_members(id: &str, role: &str, iface: &str) -> Vec<(String, String)> {
    let path = persona::CapturedPersona::select(Some(id)).unwrap().artifact(role);
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))).unwrap();
    v["interfaces"][iface]["members"]
        .as_array()
        .unwrap_or_else(|| panic!("{id}: {role} records no {iface}"))
        .iter()
        .map(|m| (m["name"].as_str().unwrap().to_string(), m["kind"].as_str().unwrap().to_string()))
        .collect()
}

/// The `describe_own` line the installer must produce for a member, from the fixture's kind. An
/// accessor is a readonly getter (`s=0`); a method is a writable data property. Both enumerable +
/// configurable, as WebIDL specifies for interface-prototype members.
fn expected_line(name: &str, kind: &str) -> String {
    match kind {
        "accessor" => format!("{name}:accessor:e=1:c=1:s=0"),
        "method" => format!("{name}:data:e=1:c=1:w=1"),
        other => panic!("unexpected fixture kind {other}"),
    }
}

/// Build the table under `id`'s surface for `realm`, exposed as `iface_name`, and compare the
/// prototype the installer produced with what that bundle recorded.
fn check(id: &str, realm: spec::SurfaceRealm, iface_name: &str, members: &[(String, String)]) {
    let mut rt = engine::new_runtime(RuntimeOptions::default());
    let ctx = rt.main_context();
    v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
    let global = scope.get_current_context().global(scope);
    // The bundle this bare runtime wears, stated. There is no persona in a `new_runtime`, which is
    // exactly why the seam exists: the engine reads the bundle it is given, and a test is the only
    // thing that gets to name one.
    webapi::install::surface::seed(scope, realm, Some(surface_of(id, realm)));

    let mut ledger = InstallLedger::new();
    let built = installer::create_interface(scope, global, &NAVIGATOR, iface_name, None, None, Flavor::Table, &mut ledger);
    assert!(
        built.gaps.is_empty(),
        "{id} / {iface_name}: the capture lists {} member(s) the table has no row for: {:?}",
        built.gaps.len(),
        built.gaps.iter().map(|g| &g.member).collect::<Vec<_>>()
    );

    let got = describe_own(scope, built.proto);
    let mut expected: Vec<String> = members.iter().map(|(n, k)| expected_line(n, k)).collect();
    // `constructor` AT THE CAPTURE'S SEAT, non-enumerable. `Navigator` is the interface
    // that makes the rule visible: the capture puts `constructor` at 32 of its 66 window members on
    // brave-151 and at 31 of 79 on chrome-150, because everything past it comes from a PARTIAL
    // interface. This line used to append it, which was right on 83 % of the surface and wrong here.
    // The WORKER capture records no position (the field was added to the window artifact only), so
    // `None` keeps the placement the installer has always given a hand table: last.
    let seat = surface_of(id, realm).members_of(iface_name).and_then(|o| o.ctor_index);
    let at = seat.unwrap_or(expected.len()).min(expected.len());
    expected.insert(at, "constructor:data:e=0:c=1:w=1".to_string());

    assert_eq!(got, expected, "{id} / {iface_name} prototype shape != that bundle's captured surface");
}

#[test]
fn navigator_window_surface_is_byte_identical_to_every_bundles_capture() {
    for id in bundles() {
        let members = fixture_members(&id, "surface", "Navigator");
        assert!(members.len() > 50, "{id}: only {} window Navigator members -- the fixture did not parse", members.len());
        check(&id, spec::SurfaceRealm::Window, "Navigator", &members);
    }
}

#[test]
fn worker_navigator_surface_is_byte_identical_to_every_bundles_capture() {
    for id in bundles() {
        let members = fixture_members(&id, "worker-surface", "WorkerNavigator");
        assert!(members.len() > 15, "{id}: only {} worker Navigator members -- the fixture did not parse", members.len());
        check(&id, spec::SurfaceRealm::Worker, "WorkerNavigator", &members);
    }
}
