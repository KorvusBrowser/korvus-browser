//! The bundle manifest — every artifact that describes one device, and where each came from.
//!
//! # Why this type exists
//!
//! A persona used to be a file of VALUES. Everything else a page can observe -- which interfaces
//! exist and in what order, what an error message says, which codecs play, how wide a glyph is,
//! what the canvas rasterises to, what the ClientHello looks like -- came from a different artifact
//! with its own provenance, committed beside whichever test used it and captured on whatever
//! machine was convenient. The `Persona` trait had no slot for any of them, nothing tied them
//! together, and the one audit that existed inspected only the object holding the values. So the
//! browser wore five devices at once and passed its own coherence check.
//!
//! A `Bundle` is that tie: the capture's identity, the artifacts that belong to it with the
//! provenance each artifact ITSELF records, the wire fingerprint it asks the transport for, and --
//! the load-bearing part -- the list of disagreements that are known and still open.
//!
//! # The allowlist is the mechanism
//!
//! The boot audit fails on any incoherence NOT in `known_incoherence`. That is what makes this
//! useful rather than decorative: today's tree really is mixed, so a plain "fail on any
//! disagreement" would refuse to start, and a plain "warn" would be the printed line nobody reads
//! that we already had. Listing them makes each one countable, gives each a stated fix, and makes
//! the SIXTH one fatal. Closing a gap means deleting its entry -- the only direction the list is
//! allowed to move without a decision.

use serde::Deserialize;

/// One device's capture identity. Two artifacts disagreeing on any of these is an incoherent bundle.
#[derive(Debug, Clone, Deserialize)]
pub struct Capture {
    pub id: String,
    pub engine: String,
    pub browser: String,
    pub version: u32,
    pub os: String,
    #[serde(default)]
    pub cpu: String,
    pub headless: bool,
    /// `"on"`, `"off"`, or `"unknown"` — a string rather than a bool because "we did not record it"
    /// is a real and different answer from "it was off", and conflating them is how a capture comes
    /// to claim a protection state nobody observed.
    ///
    /// **READ BY [`Capture::farbling_was_off`], which [`Bundle::self_check`] uses.** Once
    /// it was written by `promote.py` and consulted by nothing, which is the state the literal scan calls
    /// prose. What consults it now is the rule below, and the rule is the reason the field has to
    /// exist rather than a use invented to keep it:
    ///
    /// A bundle's `values.json` is the INPUT that a farbling policy transforms. The canvas deltas
    /// are `observed ^ 1` against the captured pixels, the audio fudge is a factor times the
    /// captured render, `hardwareConcurrency` is drawn from `[2, the captured core count]`, and
    /// `navigator.languages` is a REDUCTION of the captured list. Every one of those needs the
    /// capture to be the browser's UNPERTURBED answer. A bundle captured with fingerprint
    /// protection ON carries one session's dice roll instead, and farbling that is a policy applied
    /// on top of an already-farbled base is not a browser at all — it is noise twice, and no test
    /// downstream could tell, because both layers are supposed to look random.
    #[serde(default)]
    pub farbling: String,
    #[serde(default)]
    pub captured_at: String,
    #[serde(default)]
    pub captured_on: String,
    pub ua: String,
}

impl Capture {
    /// Was fingerprint protection OFF when this capture was taken?
    ///
    /// ONE parser for the field, deliberately. `collect.js` writes one of two sentences
    /// (`"off (raw pixels)"` / `"on (readback perturbed)"`) from a measurement — it draws the same
    /// two pixels into two canvases and compares — and `promote.py` copies the verdict through. A
    /// second reader that matched on its own spelling would be a second definition of the same
    /// fact, and the two would disagree the first time the sentence was reworded.
    ///
    /// `None` = the capture did not record it, which is NOT "it was off". A bundle that cannot say
    /// what its protection state was cannot be checked, and saying so is the whole reason the field
    /// is a string.
    pub fn farbling_was_off(&self) -> Option<bool> {
        let v = self.farbling.trim().to_ascii_lowercase();
        if v.starts_with("off") {
            Some(true)
        } else if v.starts_with("on") {
            Some(false)
        } else {
            None
        }
    }
}

/// One recorded artifact and the provenance it carries.
#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    /// `values` · `surface` · `worker-surface` · `oracle` · `codecs` · `render-reference` ·
    /// `font-metrics` · `ua-stylesheet`.
    pub role: String,
    /// Relative to the bundle's directory (`plugins/korvus-persona/bundles/<id>/`). Checked for
    /// existence at load: a manifest that names a file which has moved is a manifest that has stopped
    /// describing anything, which is the failure this whole type is about. An artifact recorded from
    /// this capture (`matches_capture`) must resolve INSIDE the directory -- the bundle is one
    /// directory; only an inherited artifact may point outside it.
    pub path: String,
    /// What the artifact's own metadata says, verbatim — not what we would prefer it said.
    pub provenance: String,
    /// `false` = taken from a different machine or session than `[capture]`, and there must be a
    /// `known_incoherence` covering it.
    pub matches_capture: bool,
}

/// A disagreement that is known, open, and deliberately not fatal — with its reason and its fix.
#[derive(Debug, Clone, Deserialize)]
pub struct KnownIncoherence {
    pub id: String,
    pub layers: String,
    pub detail: String,
    /// Why it is still here. An entry without one is a defect being laundered.
    pub why: String,
    /// What would close it. Same.
    pub fix: String,
}

/// The PRIORITY fields this capture's browser was MEASURED writing into its HTTP/2 HEADERS frame.
///
/// `weight` is RFC 9113 §5.3.2's 1..=256, the number a fingerprinter quotes, not the wire byte.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct HeadersPriority {
    #[serde(default)]
    pub stream: u32,
    pub weight: u16,
    #[serde(default)]
    pub exclusive: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Transport {
    /// The emulation preset the transport must use. `korvus-transport` looks this up and REFUSES TO
    /// START if it has no mapping, rather than silently emitting a different browser's ClientHello.
    pub emulation: String,
    /// What the captured browser ITSELF puts in the HEADERS frame's priority fields, read off a
    /// socket, when somebody has read it. Absent = not measured, and the preset's own value goes out.
    ///
    /// It is a manifest field and not a `values.json` one because no page can observe its own HTTP/2
    /// frames: this is measured with a TLS/h2 sink, the way the ClientHello is, and it is recorded
    /// beside the preset whose value it corrects.
    #[serde(default)]
    pub headers_priority: Option<HeadersPriority>,
    /// The HTTP/3 preset the captured browser was measured speaking off a QUIC sink (H3, 2026-10-11), e.g.
    /// `chrome_150`. Absent = not measured, and the persona speaks no HTTP/3.
    #[serde(default)]
    pub quic: Option<String>,
    /// The ClientHello's `trust_anchors` extension body (hex), read off a TLS sink (Chrome 152 sends one). Absent =
    /// not measured, and the ClientHello carries none.
    #[serde(default)]
    pub trust_anchors: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Bundle {
    pub capture: Capture,
    #[serde(default, rename = "artifact")]
    pub artifacts: Vec<Artifact>,
    pub transport: Transport,
    #[serde(default, rename = "known_incoherence")]
    pub known_incoherences: Vec<KnownIncoherence>,
}

impl Bundle {
    pub fn parse(manifest_toml: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(manifest_toml)
    }

    /// Is `id` a disagreement this bundle has already accounted for?
    pub fn tolerates(&self, id: &str) -> bool {
        self.known_incoherences.iter().any(|k| k.id == id)
    }

    /// The artifacts whose provenance does not match `[capture]`. Every one of these must have a
    /// `known_incoherence`, which [`Bundle::self_check`] enforces.
    pub fn foreign_artifacts(&self) -> impl Iterator<Item = &Artifact> {
        self.artifacts.iter().filter(|a| !a.matches_capture)
    }

    /// Problems with the MANIFEST itself, before anything is loaded from it.
    ///
    /// Separate from the coherence audit on purpose: that one asks whether the device is plausible,
    /// this one asks whether the manifest still describes reality. A manifest that names a moved
    /// file, or admits a foreign artifact without accounting for it, has stopped being a record --
    /// and a record nobody validates decays into the prose it replaced.
    pub fn self_check(&self, repo_root: &std::path::Path) -> Vec<String> {
        let mut bad = Vec::new();
        let bundle_dir = repo_root.join("plugins/korvus-persona/bundles").join(&self.capture.id);
        let inside = std::fs::canonicalize(&bundle_dir).ok();
        for a in &self.artifacts {
            let p = bundle_dir.join(&a.path);
            if !p.exists() {
                bad.push(format!("artifact {:?} names a path that does not exist (relative to {}): {}", a.role, bundle_dir.display(), a.path));
                continue;
            }
            if a.matches_capture {
                let canon = std::fs::canonicalize(&p).ok();
                let ok = matches!((&inside, &canon), (Some(b), Some(c)) if c.starts_with(b));
                if !ok {
                    bad.push(format!("artifact {:?} claims this capture but lives outside the bundle directory: {}", a.role, a.path));
                }
            }
        }
        // A foreign artifact with no entry explaining it is exactly the state this file was written
        // to make impossible, so it is checked here rather than trusted to review.
        for a in self.foreign_artifacts() {
            let covered = self.known_incoherences.iter().any(|k| {
                k.detail.to_lowercase().contains(&a.role.replace('-', " "))
                    || k.id.starts_with(a.role.split('-').next().unwrap_or(&a.role))
            });
            if !covered {
                bad.push(format!(
                    "artifact {:?} is marked matches_capture=false but no known_incoherence covers it",
                    a.role
                ));
            }
        }
        // A SAMPLED artifact transforms the captured values, so the captured values must be the
        // browser's unperturbed answer. See `Capture::farbling`: a policy applied on top of an
        // already-farbled base is noise twice and nothing downstream can tell.
        if self.artifacts.iter().any(|a| a.role == "farbling") {
            match self.capture.farbling_was_off() {
                Some(true) => {}
                Some(false) => bad.push(format!(
                    "this bundle carries a `farbling` policy artifact, but its capture was taken \
                     with fingerprint protection ON ({:?}) — the values that policy transforms are \
                     one session's dice roll, not this device's answer",
                    self.capture.farbling
                )),
                None => bad.push(format!(
                    "this bundle carries a `farbling` policy artifact, but its capture does not \
                     record whether fingerprint protection was on ({:?}) — \"not recorded\" is not \
                     \"it was off\"",
                    self.capture.farbling
                )),
            }
        }
        for k in &self.known_incoherences {
            if k.why.trim().is_empty() || k.fix.trim().is_empty() {
                bad.push(format!("known_incoherence {:?} has no why/fix — that is a defect being laundered", k.id));
            }
        }
        bad
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = include_str!("../bundles/chrome-152-linux-20261016T1022/manifest.toml");

    fn repo_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn the_shipped_manifest_parses_and_describes_reality() {
        let b = Bundle::parse(MANIFEST).expect("manifest parses");
        assert_eq!(b.capture.id, "chrome-152-linux-20261016T1022");
        assert_eq!(b.capture.version, 152);
        let bad = b.self_check(&repo_root());
        assert!(bad.is_empty(), "manifest does not describe reality: {bad:#?}");
    }

    /// The list is a ratchet: entries come off when a gap closes, and a new one is a decision, not a
    /// side effect. Pinning the count is what makes adding one visible in review.
    #[test]
    fn the_known_incoherences_are_the_ones_we_have_accounted_for() {
        let b = Bundle::parse(MANIFEST).unwrap();
        let mut ids: Vec<&str> = b.known_incoherences.iter().map(|k| k.id.as_str()).collect();
        ids.sort();
        assert_eq!(
            ids,
            [
                // The four `*-measured` entries the Chrome 150 bundle carried (computed-style, css-named-properties,
                // webrtc-capabilities, webrtc-offer: measured off the installed browser because that capture predated
                // collect.js recording them) closed with the Chrome 152 capture (2026-10-16), which recorded all four.
                "tls-version",
                "ua-stylesheet-inherited",
                "webgl-pixels",
            ],
            "the known-incoherence list changed. Removing one because it was FIXED is the point; \
             adding one needs a reason in the manifest and a line in the commit."
        );
    }

    /// The `farbling` field's reader, and the reason it has one (ruling R19).
    ///
    /// Asserted as the RULE and not as the shipped manifest's answer: the first version of this
    /// test read `farbling_was_off()` off `MANIFEST` and asserted `Some(true)`, which passes today
    /// and would pass just as well if `self_check` stopped consulting the field at all. What has to
    /// hold is the COUPLING — a sampled policy needs an unfarbled base — so the test builds the two
    /// manifests that must fail and checks that they do.
    /// The shipped (Chrome) manifest with a farbling artifact added -- Brave 151's captured policy, kept as test data
    /// (`testdata/brave-151-farbling.json`) -- because the rule below is about a manifest that carries one, and the
    /// shipped bundle does not.
    fn with_farbling() -> String {
        format!("{MANIFEST}\n[[artifact]]\nrole = \"farbling\"\npath = \"../../testdata/brave-151-farbling.json\"\n\
                 provenance = \"brave-151-linux-20260907T1006\"\nmatches_capture = false\n\n\
                 [[known_incoherence]]\nid = \"farbling-inherited\"\nlayers = \"capture <-> farbling\"\n\
                 detail = \"Test data: Brave 151's farbling policy on the Chrome manifest.\"\n\
                 why = \"The coupling rule is about a manifest that carries a farbling artifact.\"\nfix = \"None: a test.\"\n")
    }

    #[test]
    fn a_farbling_policy_needs_a_capture_that_was_not_itself_farbled() {
        let manifest = with_farbling();
        let b = Bundle::parse(&manifest).unwrap();
        assert!(b.artifacts.iter().any(|a| a.role == "farbling"),
                "this test is vacuous unless the shipped bundle actually carries the artifact");
        assert_eq!(b.capture.farbling_was_off(), Some(true));
        let bad = b.self_check(&repo_root());
        assert!(bad.is_empty(), "{bad:?}");

        for (field, want) in [
            ("on (readback perturbed)", "fingerprint protection ON"),
            ("", "does not record whether"),
            ("unknown", "does not record whether"),
        ] {
            let m = manifest.replace("farbling    = \"off (raw pixels)\"",
                                     &format!("farbling    = \"{field}\""));
            assert_ne!(m, manifest, "the substitution did not apply — the field was reworded");
            let bad = Bundle::parse(&m).unwrap().self_check(&repo_root());
            assert!(bad.iter().any(|s| s.contains(want)),
                    "farbling = {field:?} should have been refused; got {bad:?}");
        }
    }

    /// The other half: the rule fires on the ARTIFACT's presence, not on the browser's name. A
    /// bundle with no sampled policy is free to say anything about its own capture conditions,
    /// because there is nothing whose input it would be lying about.
    #[test]
    fn a_bundle_without_a_farbling_artifact_is_not_asked_about_farbling() {
        // the shipped Chrome bundle has no farbling artifact; its capture conditions may say anything about farbling
        let m = MANIFEST.replace("farbling    = \"off (raw pixels)\"", "farbling    = \"unknown\"");
        let b = Bundle::parse(&m).unwrap();
        assert!(!b.artifacts.iter().any(|a| a.role == "farbling"), "the artifact block was not removed");
        assert!(b.self_check(&repo_root()).is_empty(), "{:?}", b.self_check(&repo_root()));
    }

    #[test]
    fn every_foreign_artifact_is_accounted_for() {
        let b = Bundle::parse(MANIFEST).unwrap();
        let foreign: Vec<&str> = b.foreign_artifacts().map(|a| a.role.as_str()).collect();
        // FIVE, down from seven: `font-metrics` and `render-reference` are emitted from this
        // capture now. The number is asserted rather than described so that closing one is visible
        // here, and so that it can only go DOWN.
        // Ratchet: 7 -> 5 -> 3 -> 1. `render-reference` and `font-metrics` closed first; `oracle`
        // and `codecs` next; then the two surfaces, each by an emitter that reads the capture the
        // bundle was promoted from. What remains is `ua-stylesheet`, which no capture can emit --
        // its entry says why -- so 1 is the floor until something can record a sheet.
        // 1 -> 2 on 2026-10-04 (ACCESS): `computed-style` was measured off the installed browser, since this
        // capture predates the field; `emit.py --role computed-style` over a fresh capture takes it back to 1.
        // 2 -> 3 on 2026-10-07: `webrtc-offer` was measured off the installed browser for the same
        // reason. The alternative was Chrome 150's offer compiled into core/ for every persona -- one browser's
        // output in the engine, and `corpus_hostnames` 1 -> 3 over its header-extension URIs. `emit.py --role
        // webrtc-offer` over a fresh capture takes it back down.
        // 3 -> 5 on 2026-10-08: `webrtc-capabilities` (getCapabilities answered null for every kind)
        // and `css-named-properties` (a declaration named no camel-cased property as its own), measured off the
        // installed browser the same way; `emit.py --role <role>` over a fresh capture takes each back down.
        // 5 -> 1 on 2026-10-16: the Chrome 152 capture recorded those four itself (`emit.py --role` each), and
        // `ua-stylesheet` is the floor again.
        assert_eq!(foreign.len(), 1, "foreign artifacts: {foreign:?}");
    }

    #[test]
    fn a_captured_artifact_outside_the_bundle_directory_fails_its_own_check() {
        let mut m = MANIFEST.to_string();
        m.push_str("\n[[artifact]]\nrole = \"escaped\"\npath = \"../../src/lib.rs\"\nprovenance = \"x\"\nmatches_capture = true\n");
        let b = Bundle::parse(&m).unwrap();
        let bad = b.self_check(&repo_root());
        assert!(bad.iter().any(|s| s.contains("outside the bundle directory")), "{bad:?}");
    }

    #[test]
    fn a_manifest_that_names_a_missing_file_fails_its_own_check() {
        let mut m = MANIFEST.to_string();
        m.push_str("\n[[artifact]]\nrole = \"invented\"\npath = \"no/such/file.json\"\nprovenance = \"x\"\nmatches_capture = true\n");
        let b = Bundle::parse(&m).unwrap();
        let bad = b.self_check(&repo_root());
        assert!(bad.iter().any(|s| s.contains("does not exist")), "{bad:?}");
    }
}
