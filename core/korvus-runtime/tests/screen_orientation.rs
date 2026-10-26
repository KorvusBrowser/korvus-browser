//! `screen.orientation` answers the WORN BUNDLE's `ScreenOrientation`.
//!
//! EVERY ASSERTION HERE IS A RULE, NOT AN OBSERVATION. The four bundles in this tree all captured
//! `landscape-primary` / 0, so a test that spelled that string would pass just as happily on an
//! engine with the string compiled in -- and would go on passing after the next capture is taken on
//! a phone. So:
//!
//!   * `every_bundle_answers_its_own_captured_orientation` reads each bundle's `values.json`
//!     independently of the engine and demands the page answer THAT, whatever it says.
//!   * `a_persona_that_captured_another_orientation_answers_that_one` wears a portrait persona no
//!     bundle in this tree carries. An engine with a constant fails it; an engine that reads the
//!     profile cannot.
//!   * `a_persona_that_captured_no_orientation_exposes_none` pins the other half of the profile
//!     rule: an uncaptured orientation is ABSENT, never invented.
//!   * `orientation_is_seated_where_the_capture_puts_it` grades the member's position against the
//!     capture's own recorded order rather than against an index typed here. The order ratchet
//!     counts GLOBALS and cannot see a member, which is how this row sat after `constructor`
//!     answering `undefined`.
//!
//! The device answers this lane matched, measured on real Brave 151.0.7922.173 over CDP, headed
//! under Xvfb, 2026-09-19 (the full table is in `webapi/src/core/html/screen_orientation.rs`):
//! `[object ScreenOrientation]`, own properties `[]`, one object per realm, `onchange` null and
//! settable, `unlock()` -> `undefined`.

use engine::Engine;
use spec::Persona;

/// Every bundle directory that carries a `values.json`, sorted. The denominator is the tree, not a
/// list here: a bundle added without an orientation is exactly what these rules exist to catch.
fn bundle_ids() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/korvus-persona/bundles");
    let mut out: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("values.json").is_file())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no persona bundle in the tree");
    out
}

/// `screen.orientation` as the bundle's OWN `values.json` records it -- read straight from the JSON,
/// not through `ScreenProfile`, so the whole chain (capture -> loader -> profile -> page) is under
/// test and not just its last link.
fn captured_orientation(id: &str) -> Option<(String, f64)> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/korvus-persona/bundles")
        .join(id)
        .join("values.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))).unwrap();
    let o = v.get("screen")?.get("orientation")?;
    Some((o.get("type")?.as_str()?.to_string(), o.get("angle")?.as_f64()?))
}

/// Boot an engine wearing `p` and read the three answers this lane owns.
async fn orientation_of(p: Box<dyn Persona>) -> Vec<String> {
    let mut e = Engine::new(p);
    e.run().await.unwrap();
    e.eval("fp82", r#"
        const o = screen.orientation;
        print("tag=" + Object.prototype.toString.call(o));
        print("type=" + (o && o.type));
        print("angle=" + (o && o.angle));
    "#.into()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

#[tokio::test]
async fn every_bundle_answers_its_own_captured_orientation() {
    for id in bundle_ids() {
        let p = persona::CapturedPersona::select(Some(&id)).unwrap_or_else(|e| panic!("select {id}: {e}"));
        // The bundle is the expectation, whatever it says -- this file never spells an orientation.
        let (ty, angle) = captured_orientation(&id)
            .unwrap_or_else(|| panic!("{id}: values.json records no screen.orientation; a capture without one \
                                       cannot be graded, and the engine must expose no orientation for it"));
        let prof = Persona::screen(&p);
        assert_eq!(prof.orientation_type, ty, "{id}: the profile lost the capture's type");
        assert_eq!(prof.orientation_angle as f64, angle, "{id}: the profile lost the capture's angle");
        assert_eq!(
            orientation_of(persona::worn_on_host(p)).await,
            vec!["tag=[object ScreenOrientation]".to_string(), format!("type={ty}"), format!("angle={angle}")],
            "{id}: the page does not answer the bundle's own screen.orientation"
        );
    }
}

/// THE CONSTANT-CATCHER. A persona whose orientation no bundle in this tree captured: portrait, at
/// 90 degrees. Four desktop Linux captures agreeing on `landscape-primary` is a coincidence of this
/// tree, so the rule is tested where the coincidence cannot help.
#[tokio::test]
async fn a_persona_that_captured_another_orientation_answers_that_one() {
    let base = persona::CapturedPersona::default();
    let mut s = Persona::screen(&base);
    assert_ne!(s.orientation_type, "portrait-primary", "the shipped bundle must not already be the test's value");
    s.orientation_type = "portrait-primary".to_string();
    s.orientation_angle = 90;
    let p = persona::Overridden::on(base).screen(s).boxed();
    assert_eq!(
        orientation_of(p).await,
        vec!["tag=[object ScreenOrientation]", "type=portrait-primary", "angle=90"],
        "screen.orientation does not come from the profile -- it is a constant in the engine"
    );
}

/// The other half of the profile rule (`spec::ScreenProfile`): a capture that recorded no
/// orientation gets NO orientation object. `width > height` is a guess about a device, not a reading
/// of one, so there is nothing to fall back to.
#[tokio::test]
async fn a_persona_that_captured_no_orientation_exposes_none() {
    let base = persona::CapturedPersona::default();
    let mut s = Persona::screen(&base);
    s.orientation_type = String::new();
    s.orientation_angle = 0;
    let p = persona::Overridden::on(base).screen(s).boxed();
    assert_eq!(
        orientation_of(p).await,
        vec!["tag=[object Undefined]", "type=undefined", "angle=undefined"],
        "an uncaptured orientation was invented rather than left absent"
    );
}

/// The SEAT, graded against the capture's own recorded member order.
///
/// `orientation` belongs between `availTop` and `constructor`: the capture puts it at index 8 with
/// `constructor` at 9 and the two partial-interface members (`onchange`, `isExtended`) after. It used
/// to land after `constructor`, because the registry's backfill only ADDS what a prototype does not
/// already own and the native `Screen` class owns everything up to `constructor` -- a position error
/// no ratchet in this tree would have caught, since the order ratchet counts globals.
#[tokio::test]
async fn orientation_is_seated_where_the_capture_puts_it() {
    for id in bundle_ids() {
        let p = persona::CapturedPersona::select(Some(&id)).unwrap_or_else(|e| panic!("select {id}: {e}"));
        let captured = Persona::surface(&p, spec::SurfaceRealm::Window)
            .unwrap_or_else(|| panic!("{id} records no window surface"))
            .members_of("Screen")
            .map(|m| m.members.clone())
            .unwrap_or_else(|| panic!("{id}: the capture records no Screen members"));
        let at = |names: &[String], n: &str| names.iter().position(|x| x == n);
        let (c_top, c_or) = (at(&captured, "availTop"), at(&captured, "orientation"));
        assert_eq!(
            c_or, c_top.map(|i| i + 1),
            "{id}: the CAPTURE does not put orientation straight after availTop ({captured:?}) -- \
             this test's premise is the capture's, so re-read it before touching the engine"
        );

        let mut e = Engine::new(persona::worn_on_host(p));
        e.run().await.unwrap();
        e.eval("seat", r#"print(Object.getOwnPropertyNames(Screen.prototype).join(","))"#.into()).unwrap();
        e.run().await.unwrap();
        let live: Vec<String> = e.take_output()[0].split(',').map(|s| s.to_string()).collect();
        let (top, or, ctor) = (at(&live, "availTop"), at(&live, "orientation"), at(&live, "constructor"));
        assert_eq!(or, top.map(|i| i + 1), "{id}: orientation is not seated straight after availTop: {live:?}");
        assert!(or < ctor, "{id}: orientation is seated AFTER constructor: {live:?}");
    }
}
