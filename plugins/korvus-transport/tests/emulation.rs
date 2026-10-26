//! `emulation_for` resolves what the pinned library can emit, and refuses everything else.
//!
//! THE LIST IS NEVER TYPED HERE. Every assertion below is computed from `wreq_util::Profile`'s own
//! preset table, because the defect this file exists to prevent is a mapping that drifts from the
//! library: the hand-written match this replaced had 40 Chrome arms for the library's 41 and silently
//! lacked `chrome_110`. A test that enumerated a list a test author typed would have agreed with the
//! bug. L7f's Edge and Opera bundles are the reason it matters now -- they name presets no bundle in
//! the tree names yet, and this is what guarantees the transport can already reach them.

/// Every preset `wreq-util` ships resolves, and resolves to the variant whose name it is.
///
/// This is the edge/opera requirement: the two families are in `Profile::VARIANTS`, so they are in
/// this loop, and no arm had to be written for either.
#[test]
fn every_preset_the_library_ships_resolves() {
    let names = transport::profile_names();
    // NOT A PINNED COUNT -- the library's own variant list is the denominator. `profile_names`
    // reads each variant's name through serde and DROPS any that does not come back as a string, so
    // a variant the derive stopped covering would shrink the list silently and make every assertion
    // below vacuous over a smaller set. This is the check that cannot be fooled that way.
    assert_eq!(
        names.len(),
        wreq_util::Profile::VARIANTS.len(),
        "profile_names() lost variants: {} names for {} variants",
        names.len(),
        wreq_util::Profile::VARIANTS.len()
    );
    assert!(names.len() > 100, "an empty or tiny preset list makes this whole file vacuous");

    let mut unresolved = Vec::new();
    let mut wrong = Vec::new();
    for (name, expected) in &names {
        match transport::emulation_for(name) {
            Ok(got) if got == *expected => {}
            Ok(got) => wrong.push(format!("{name} -> {got:?}, expected {expected:?}")),
            Err(e) => unresolved.push(format!("{name}: {e}")),
        }
    }
    assert!(unresolved.is_empty(), "presets the library ships but the transport cannot reach: {unresolved:#?}");
    assert!(wrong.is_empty(), "presets that resolved to the WRONG variant: {wrong:#?}");
}

/// The Chromium family is reachable in all three of its shapes, and by more than one member each.
///
/// A count rather than a name list: the point is that the transport reaches the whole family, and a
/// specific version here would be a hand list by another spelling. Prefix grouping is a property of
/// the LIBRARY'S names, read in a test; `emulation_for` itself never asks which family a name is in.
#[test]
fn the_chromium_family_is_reachable() {
    let names = transport::profile_names();
    for family in ["chrome_", "edge_", "opera_"] {
        let members: Vec<_> = names.iter().map(|(n, _)| n.as_str()).filter(|n| n.starts_with(family)).collect();
        assert!(
            members.len() > 1,
            "wreq-util should ship several {family}* presets; found {members:?}"
        );
        for name in &members {
            assert!(
                transport::emulation_for(name).is_ok(),
                "{name} is a {family}* preset the library ships and the transport refused it"
            );
        }
    }
}

/// THE NO-SILENT-FALLBACK PROPERTY, ASSERTED. An id the library does not have is an `Err`.
///
/// This is the reason `emulation_for` returns a Result at all. A fallback here would put some other
/// browser's ClientHello under this persona's UA and nothing downstream would ever notice, which is
/// the whole defect class this crate exists to prevent -- so it is checked for an empty id, a typo, a
/// plausible-but-absent version, and a preset from a library that is not the pinned one.
#[test]
fn an_unknown_preset_is_an_error_not_a_fallback() {
    // `chrome_150`/`chrome_151` are the real ones: both shipped bundles' UA claims a version the
    // pinned wreq-util has no preset for, which is the `tls-version` known_incoherence. The right
    // answer is still a refusal -- see `clienthello.rs` for the measurement behind that entry.
    for bad in ["", "chrome", "chrome_", "chrome_150", "chrome_151", "chrome_1490", "Chrome149", "chrome-149", "brave_151", "nonsense"] {
        let r = transport::emulation_for(bad);
        assert!(r.is_err(), "{bad:?} must not resolve -- a fallback wears another browser's wire");
        let msg = r.unwrap_err().to_string();
        assert!(
            msg.contains("Refusing to start"),
            "the refusal for {bad:?} must say it refuses rather than substitutes; got: {msg}"
        );
    }
}

/// The refusal carries the names that WOULD have worked.
///
/// `emulation_for` is fail-closed, so its error text is the only thing an operator with a wrong
/// manifest ever sees. "That name is wrong" without "these are the names" costs a round.
#[test]
fn the_refusal_lists_what_is_available() {
    let msg = transport::emulation_for("chrome_151").unwrap_err().to_string();
    for present in ["chrome_149", "edge_148", "opera_131"] {
        assert!(msg.contains(present), "the refusal should name {present} as available; got: {msg}");
    }
}

/// `chrome_110` -- the arm the hand-written table was missing.
///
/// Kept as its own named case, not folded into the loop above, because a regression to a typed list
/// would most likely reintroduce exactly this gap: it is the one Chrome version that breaks the
/// otherwise-contiguous run, and it is what proved the old mapping was rotting.
#[test]
fn chrome_110_resolves() {
    assert!(
        transport::emulation_for("chrome_110").is_ok(),
        "wreq-util ships chrome_110; the hand-written match arms did not, for no recorded reason"
    );
}

/// Both shipped bundles declare a preset this transport can actually emit.
///
/// Reads the manifests rather than a copy of what they say, so a promotion that stamped an
/// unresolvable preset fails here instead of at the first fetch of a real run.
#[test]
fn every_shipped_bundle_declares_a_resolvable_preset() {
    let bundles = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../korvus-persona/bundles")
        .canonicalize()
        .expect("bundles dir");
    let mut checked = 0;
    for entry in std::fs::read_dir(&bundles).expect("read bundles/") {
        let dir = entry.expect("dir entry").path();
        let manifest = dir.join("manifest.toml");
        if !manifest.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&manifest).expect("read manifest");
        let id = text
            .lines()
            .skip_while(|l| l.trim() != "[transport]")
            .find_map(|l| l.trim().strip_prefix("emulation"))
            .and_then(|l| l.split('"').nth(1).map(str::to_string))
            .unwrap_or_else(|| panic!("{} declares no [transport] emulation", manifest.display()));
        transport::emulation_for(&id)
            .unwrap_or_else(|e| panic!("{} declares emulation {id:?}: {e}", manifest.display()));
        checked += 1;
    }
    assert!(checked >= 1, "no persona bundle in the tree");
}
