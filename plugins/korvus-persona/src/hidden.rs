//! The objects a browser exposes WITHOUT an interface object behind them: `navigator.brave`
//! and the window's proprietary namespaces `opr` / `g_opr`.
//!
//! # Why these three strings live here and not in the engine
//!
//! `navigator.brave` hands back an object whose hidden constructor is named `Brave`, whose
//! `Symbol.toStringTag` is `Brave`, and whose one method `isBrave()` resolves `true`. Until L7e all
//! three were spelled in `core/korvus-webapi/src/core/navigator/devices.rs`. The ROW was already
//! conditional — L7b/L7c made presence follow the active capture, so a Chrome bundle produced no
//! `navigator.brave` at all — but the STRINGS were still compiled into the engine, and P2 is about
//! the strings: *no browser-identifying literal in engine source*, so that swapping the persona swaps
//! the whole fingerprint and not most of it. `scripts/persona_literals.py` is the ratchet that keeps
//! them out; this module is where they are allowed to be.
//!
//! # Why the capture cannot supply them, and what does instead
//!
//! The rig records that the MEMBER exists: `surface.json`'s `Navigator.brave` row, which is what
//! [`descriptors_for`] reads to decide whether this device has the object at all. It does not record
//! the shape of the value the member returns — `collect.js` walks property DESCRIPTORS, and the
//! object behind an accessor is not one. So the shape is this crate's own knowledge about a browser,
//! which is exactly what a persona crate is for, and it is gated on the capture so that a bundle
//! without `Navigator.brave` never produces it.
//!
//! This is NOT a branch on which bundle is worn (the unit contract forbids that, and it forbids it in
//! ENGINE code for a reason that applies here too): nothing below reads a bundle id or
//! `KORVUS_BUNDLE`. It reads what the capture records, the same question `install::surface` asks.
//!
//! # The split with the engine
//!
//! The persona says WHICH members exist, in which order, with what arity, and WHICH of the engine's
//! native bodies answers each ([`spec::HiddenAnswer`]). The body stays a Rust callback built by
//! `binding::protos::build_fn` — invariant I3, bodies are Rust — so nothing here is a script and a
//! body the engine does not have cannot be asked for.

use spec::{HiddenAnswer, HiddenMethod, HiddenObject, RealmSurface, WindowNamespace};

/// `navigator.<member>` -> the hidden object it returns, for every such member THIS capture records.
///
/// The whole table is below, because it is one row: `window_surface` is the device's window surface,
/// and a member it does not list produces nothing.
pub fn descriptors_for(window_surface: Option<&RealmSurface>) -> Vec<HiddenObject> {
    let mut out = Vec::new();
    if records(window_surface, "Navigator", "brave") {
        out.push(HiddenObject {
            member: "brave".to_string(),
            interface: "Brave".to_string(),
            methods: vec![HiddenMethod {
                name: "isBrave".to_string(),
                fn_name: None,
                length: 0,
                answer: HiddenAnswer::ResolvedTrue,
            }],
        });
    }
    out
}

/// `window.<name>` -> the proprietary namespace object it holds, for every such global THIS capture
/// records. Two rows, both Opera's, and the whole table is below for the same reason the one above is.
///
/// WHAT THE CAPTURE GIVES AND WHAT IT CANNOT. `surface.json`'s `globals` array carries the NAMES --
/// `opr` and `g_opr` are in `opera-135-linux-20260913T2045`'s and in none of the other three -- and
/// that is the entire contribution a property-descriptor walk can make about a plain data property
/// holding a plain object. Everything else below was MEASURED on the installed Opera 135.0.5973.133
/// (headed under Xvfb, a page on 127.0.0.1 posting its answer back, 2026-09-13):
///
///   window.opr     `{}`  -- no own properties at all, `[object Object]`, `constructor === Object`,
///                  `Object.getPrototypeOf(opr) === Object.prototype`, extensible, not sealed.
///                  Own descriptor `{writable: true, enumerable: true, configurable: true}`.
///   window.g_opr   one own data property, `scrap`: a native function, `length` 0, `name` "",
///                  `String(...)` "function () { [native code] }", and CALLING it answers `undefined`.
///                  Own descriptor `{writable: true, enumerable: true, configurable: FALSE}` -- the
///                  one flag that differs between the two, which is why `WindowNamespace` carries it.
///                  `opr !== g_opr`: two distinct objects, not one under two names.
///
/// A NAMESPACE IS NOT AN INTERFACE. Neither object has an interface object on the global (`typeof Opr`
/// is `undefined` in Opera), no prototype chain of its own, and nothing a one-table row could carry --
/// which is why these are here rather than in a table, and why `spec::WindowNamespace` describes an
/// OBJECT rather than an interface.
pub fn window_namespaces_for(window_surface: Option<&RealmSurface>) -> Vec<WindowNamespace> {
    let mut out = Vec::new();
    // ORDER IS THE CAPTURE'S, and the capture's is `g_opr` before `opr`: real Opera's
    // `Object.getOwnPropertyNames(window)` reads `... Float16Array, chrome, g_opr, WebAssembly, opr,
    // crashReport, ...`, so `g_opr` is installed first. The two are not adjacent on the real window
    // and this engine cannot make them adjacent either way -- `global_order.rs` holds that whole
    // question as a measured ratchet -- but their RELATIVE order is free and is therefore right.
    if has_global(window_surface, "g_opr") {
        out.push(WindowNamespace {
            global: "g_opr".to_string(),
            members: vec![HiddenMethod {
                name: "scrap".to_string(),
                // ANONYMOUS in the real browser. Naming it after its key is the `chrome.loadTimes`
                // defect all over again, one namespace over.
                fn_name: Some(String::new()),
                length: 0,
                answer: HiddenAnswer::Undefined,
            }],
            configurable: false,
        });
    }
    if has_global(window_surface, "opr") {
        out.push(WindowNamespace { global: "opr".to_string(), members: Vec::new(), configurable: true });
    }
    out
}

/// Does this capture's realm list a global with this name? Same reading of "no surface" as
/// [`records`]: a persona with no bundle behind it has no browser-specific objects.
fn has_global(surface: Option<&RealmSurface>, name: &str) -> bool {
    surface.map(|s| s.has_global(name)).unwrap_or(false)
}

/// Does this capture list `iface.member`? `None` (no surface recorded) is "this persona has no
/// bundle behind it", and a persona with no bundle has no browser-specific objects either.
fn records(surface: Option<&RealmSurface>, iface: &str, member: &str) -> bool {
    surface
        .and_then(|s| s.members_of(iface))
        .map(|m| m.members.iter().any(|x| x == member))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both directions of the gate this module is: the object exists exactly when the capture lists
    /// the member. The bundles themselves are the fixtures — a Brave capture records `Navigator.brave`
    /// and `chrome-150-…` does not — so this is the same question `persona_surface.rs` asks of the
    /// running engine, asked here of the data one layer earlier.
    /// Every bundle directory in the tree, sorted — the denominator is the directory, never a list.
    fn bundle_ids() -> Vec<String> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bundles");
        let mut out: Vec<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("manifest.toml").is_file())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        out.sort();
        out
    }

    #[test]
    fn the_hidden_object_follows_the_capture_in_both_directions() {
        let mut with = 0;
        let mut without = 0;
        for dir in bundle_ids() {
            let p = crate::CapturedPersona::select(Some(&dir)).expect("select");
            let s = spec::Persona::surface(&p, spec::SurfaceRealm::Window);
            let listed = records(s.as_deref(), "Navigator", "brave");
            let got = descriptors_for(s.as_deref());
            if listed {
                with += 1;
                assert_eq!(got.len(), 1, "{dir}: the capture lists Navigator.brave");
                assert_eq!(got[0].interface, "Brave");
                assert_eq!(got[0].methods[0].name, "isBrave");
            } else {
                without += 1;
                assert!(got.is_empty(), "{dir}: the capture does not list Navigator.brave, got {got:?}");
            }
        }
        // Chrome only since 2026-10-16: the side WITH the member left with the Brave bundle, and comes back with a
        // Brave persona plugin. What this tree can still show is the other side, on every bundle it holds.
        assert!(without > 0 && with == 0, "{with} with, {without} without");
    }

    /// The same gate, for the window namespaces: the objects exist exactly when the capture's own
    /// globals list the names, and the shapes are the measured ones. `opera-135-…` records `opr` and
    /// `g_opr`; the other three record neither, so this tree carries both sides of the question.
    #[test]
    fn the_window_namespaces_follow_the_capture_in_both_directions() {
        let (mut with, mut without) = (0, 0);
        for dir in bundle_ids() {
            let p = crate::CapturedPersona::select(Some(&dir)).expect("select");
            let s = spec::Persona::surface(&p, spec::SurfaceRealm::Window);
            let listed = has_global(s.as_deref(), "opr");
            let got = window_namespaces_for(s.as_deref());
            if listed {
                with += 1;
                // Both names travel together in every capture that has either, so the test asserts
                // that rather than assuming it: a bundle with one and not the other would be a
                // capture worth looking at, not a case to handle.
                assert!(has_global(s.as_deref(), "g_opr"), "{dir}: records opr but not g_opr");
                assert_eq!(got.len(), 2, "{dir}: two namespaces, got {got:?}");
                assert_eq!(got[0].global, "g_opr", "{dir}: the capture's own-global order puts g_opr first");
                assert_eq!(got[0].members.len(), 1);
                assert_eq!(got[0].members[0].name, "scrap");
                assert_eq!(got[0].members[0].fn_name.as_deref(), Some(""), "scrap is anonymous in Opera");
                assert!(!got[0].configurable, "g_opr's own descriptor is configurable: false");
                assert_eq!(got[1].global, "opr");
                assert!(got[1].members.is_empty(), "{dir}: real Opera's window.opr is empty");
                assert!(got[1].configurable, "opr's own descriptor is configurable: true");
            } else {
                without += 1;
                assert!(got.is_empty(), "{dir}: the capture records no opr, got {got:?}");
                assert!(!has_global(s.as_deref(), "g_opr"), "{dir}: records g_opr but not opr");
            }
        }
        // As above: the side WITH `opr` left with the Opera bundle.
        assert!(without > 0 && with == 0, "{with} with, {without} without");
    }
}
