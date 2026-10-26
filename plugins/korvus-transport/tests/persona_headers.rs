//! **THE REQUEST HEADERS ARE THE PERSONA'S**, for the three fields that were not.
//!
//! Measured 2026-09-18 on this box, each browser in a fresh profile at `LANG=ko_KR.UTF-8`, one
//! navigation plus nineteen subresources read off a cleartext loopback socket, and the HTTP/2
//! HEADERS frame read off a loopback TLS/h2 sink whose certificate the browser was taught through an
//! isolated NSS store (never `--ignore-certificate-errors`):
//!
//! ```text
//! browser / bundle                 accept-language                       sec-gpc  h2 weight
//! Brave 151.1.93.138               ko-KR,ko;q=0.N  (N farbled per session)   1        256
//! Google Chrome 150.0.7871.46      ko-KR,ko;q=0.9,en-US;q=0.8,en;q=0.7       -        256
//! Microsoft Edge 151.0.4129.93     ko,en;q=0.9,en-US;q=0.8                   -        256
//! Opera 135.0.5973.133             ko-KR,ko;q=0.9,en-US;q=0.8,en;q=0.7       -        256
//! korvus, BEFORE, under all four   en-US,en;q=0.9  (the wreq preset's)       1        220
//! ```
//!
//! Every assertion here reads a SHIPPED BUNDLE and compares the header to that bundle's own recorded
//! values. Nothing below spells a locale, a q-value or a browser name: a test that typed `ko-KR`
//! would pass on a tree whose engine typed it too, which is the defect this lane is about.

use spec::Persona;

fn bundles_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../korvus-persona/bundles")
        .canonicalize()
        .expect("bundles dir")
}

/// Every shipped bundle, by capture id, loaded as the persona a run would wear.
fn every_bundle() -> Vec<(String, persona::CapturedPersona)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(bundles_dir()).expect("read bundles/") {
        let dir = entry.expect("dir entry").path();
        if !dir.join("manifest.toml").is_file() {
            continue;
        }
        let id = dir.file_name().expect("bundle name").to_string_lossy().to_string();
        let p = persona::CapturedPersona::select(Some(&id))
            .unwrap_or_else(|e| panic!("bundle {id}: {e}"));
        out.push((id, p));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(!out.is_empty(), "no persona bundle in the tree");
    out
}

fn header(h: &[(String, String)], name: &str) -> Option<String> {
    let mut hit = h.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name));
    let first = hit.next().map(|(_, v)| v.clone());
    assert!(hit.next().is_none(), "{name} was emitted more than once: {h:?}");
    first
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// FACT 1 — `accept-language` is the persona's, and it agrees with `navigator.languages`.
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// Each bundle's `Accept-Language` is its OWN, and it is the one the capture recorded.
///
/// Non-vacuous by construction: the four bundles do not all record the same header (Edge's list is a
/// different list from the other three's), so a transport that emitted one constant — which is
/// exactly what it did, `en-US,en;q=0.9` from the emulation preset — cannot pass this.
#[test]
fn accept_language_comes_from_the_bundle_that_is_worn() {
    let all = every_bundle();
    let mut seen = std::collections::BTreeSet::new();
    for (id, p) in &all {
        let want = p.navigator().accept_language;
        assert!(!want.is_empty(), "{id}: the bundle records no acceptLanguage");
        let got = header(&transport::persona_headers(p), "accept-language");
        assert_eq!(got.as_deref(), Some(want.as_str()), "{id}: the wire header is not the bundle's");
        seen.insert(want);
    }
    // NON-VACUITY: with one persona in the tree (Chrome, since 2026-10-16) the loop cannot tell a
    // per-persona header from a constant, so a persona that records another list must get that list.
    struct Other(persona::CapturedPersona);
    impl Persona for Other {
        fn navigator(&self) -> spec::NavigatorProfile {
            spec::NavigatorProfile { accept_language: "fr-FR,fr;q=0.9".into(), ..self.0.navigator() }
        }
        fn screen(&self) -> spec::ScreenProfile {
            self.0.screen()
        }
        fn transport_profile(&self) -> spec::TlsProfile {
            self.0.transport_profile()
        }
        fn client_hints(&self) -> spec::ClientHints {
            self.0.client_hints()
        }
    }
    assert!(!seen.contains("fr-FR,fr;q=0.9"), "{seen:?}");
    let got = header(&transport::persona_headers(&Other(persona::CapturedPersona::shipped())), "accept-language");
    assert_eq!(got.as_deref(), Some("fr-FR,fr;q=0.9"), "the wire header does not follow the persona's list");
}

/// THE TELL ITSELF: the header and `navigator.languages` are the same preference, so the header's
/// first language tag is the first entry of the list the page is told.
///
/// A request that says `en-US` while its own JS says `ko-KR` is a contradiction inside one page load,
/// and that is what the preset's value was. Only the FIRST tag is compared, because the rest is not
/// the same function in every browser: Brave reduces the list and farbles the q-value where Chromium
/// runs the whole list through its q-ladder. Asserting the whole string here would be asserting a
/// derivation this engine deliberately does not perform.
#[test]
fn the_header_and_navigator_languages_are_one_preference() {
    for (id, p) in every_bundle() {
        let nav = p.navigator();
        let first_tag = nav.accept_language.split([',', ';']).next().unwrap_or("").to_string();
        assert_eq!(
            Some(first_tag.as_str()),
            nav.languages.first().map(String::as_str),
            "{id}: the wire says {:?} and navigator.languages says {:?} -- one page load, two answers",
            nav.accept_language,
            nav.languages
        );
    }
}

/// A PAGE MAY STILL STATE IT. `accept-language` is CORS-safelisted and Chrome sends the author's
/// value on a `fetch()` that carries one, so the persona states it only when the request does not —
/// and never twice.
#[test]
fn an_author_supplied_accept_language_survives_the_persona() {
    let (id, p) = every_bundle().remove(0);
    let mut req = transport::FetchRequest::navigation("https://example-korvus.test/");
    req.headers.push(("Accept-Language".into(), "de-DE".into()));
    transport::apply_persona_headers(&p, &mut req);
    assert_eq!(
        header(&req.headers, "accept-language").as_deref(),
        Some("de-DE"),
        "{id}: a page's own accept-language must survive (and must not be doubled)"
    );

    // And with no author header, the persona's is what goes out.
    let mut bare = transport::FetchRequest::navigation("https://example-korvus.test/");
    transport::apply_persona_headers(&p, &mut bare);
    assert_eq!(
        header(&bare.headers, "accept-language").as_deref(),
        Some(p.navigator().accept_language.as_str()),
        "{id}: with nothing authored, the persona states the header"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// FACT 2 — `Sec-GPC` follows the bundle, and it follows `navigator.globalPrivacyControl`.
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// `Sec-GPC` is sent by exactly the bundles whose capture recorded the JS member as true.
///
/// Both halves are asserted against the SAME field, because the point is that they cannot disagree:
/// a browser that answers `navigator.globalPrivacyControl === true` and sends no header, or sends the
/// header while its navigator has no such member, is readable in one page load. Before FP2 the
/// header was pushed unconditionally, so three of these four bundles sent a header their browsers
/// never send.
#[test]
fn sec_gpc_is_sent_by_exactly_the_bundles_that_claim_gpc() {
    let all = every_bundle();
    let mut with = 0usize;
    for (id, p) in &all {
        let claims = p.navigator().global_privacy_control == Some(true);
        let got = header(&transport::persona_headers(p), "sec-gpc");
        assert_eq!(
            got.is_some(),
            claims,
            "{id}: navigator.globalPrivacyControl == {claims:?} but the wire header is {got:?}"
        );
        if claims {
            assert_eq!(got.as_deref(), Some(transport::GPC_HEADER_VALUE), "{id}: wrong Sec-GPC value");
            with += 1;
        }
    }
    // NON-VACUITY: a tree where every bundle claimed GPC would pass the loop above without testing the
    // negative case. The positive case is `flipping_the_gpc_claim_flips_the_header`'s: Chrome, the one
    // persona since 2026-10-16, does not claim GPC (Brave 151 did).
    assert!(with < all.len(), "every shipped bundle claims GPC, so the negative case is untested");
}

/// RED-PROOF: flip the claim on a persona and the header follows, with nothing else changed.
#[test]
fn flipping_the_gpc_claim_flips_the_header() {
    struct Flipped(persona::CapturedPersona, Option<bool>);
    impl Persona for Flipped {
        fn navigator(&self) -> spec::NavigatorProfile {
            spec::NavigatorProfile { global_privacy_control: self.1, ..self.0.navigator() }
        }
        fn screen(&self) -> spec::ScreenProfile {
            self.0.screen()
        }
        fn transport_profile(&self) -> spec::TlsProfile {
            self.0.transport_profile()
        }
        fn client_hints(&self) -> spec::ClientHints {
            self.0.client_hints()
        }
    }
    let base = persona::CapturedPersona::shipped();
    for claim in [None, Some(false), Some(true)] {
        let p = Flipped(base.clone(), claim);
        let got = header(&transport::persona_headers(&p), "sec-gpc").is_some();
        assert_eq!(got, claim == Some(true), "Sec-GPC for globalPrivacyControl = {claim:?}");
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// FACT 3 — the HTTP/2 HEADERS priority is the persona's, not the preset's.
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// Every shipped bundle declares the HEADERS-frame priority it was measured sending, and it is not
/// the pinned preset's.
///
/// The preset's own value is read out of `wreq-util` rather than typed, so this stays true if the pin
/// moves: what is asserted is that the bundle CORRECTS the preset, not that either is a magic number.
#[test]
fn every_bundle_declares_the_headers_priority_it_was_measured_sending() {
    for (id, p) in every_bundle() {
        let wire = p.transport_profile();
        let got = wire.h2_priority.unwrap_or_else(|| {
            panic!(
                "{id} declares no [transport] headers_priority. It is measured off a TLS/h2 sink, \
                 not derivable from a capture, and without it this bundle silently ships the \
                 emulation preset's weight -- which is what FP2 measured as wrong for all four \
                 browsers in this family."
            )
        });
        assert_eq!(got.dep_stream, 0, "{id}: the family depends on the connection root");
        assert!(got.exclusive, "{id}: the family sets the E bit");
        assert!((1..=256).contains(&got.weight), "{id}: weight {} is outside RFC 9113's range", got.weight);

        let preset = preset_priority(&wire.id);
        assert_ne!(
            Some(as_frame(got)),
            preset,
            "{id}: the bundle's measured priority equals the {} preset's, so nothing is being \
             corrected and this test is vacuous",
            wire.id
        );
    }
}

/// The priority the pinned `wreq-util` preset would put on the wire, read from the library's own
/// emulation rather than from a number typed here.
///
/// `StreamDependency` keeps its fields private in the pinned `http2` and exposes no weight accessor,
/// so the comparison is between two whole frame values -- which is the right granularity anyway: the
/// dependency and the E bit are as fingerprinted as the weight.
fn preset_priority(id: &str) -> Option<wreq::http2::StreamDependency> {
    use wreq::IntoEmulation;
    let em = wreq_util::Emulation::builder()
        .profile(transport::emulation_for(id).expect("the bundle's preset must resolve"))
        .build()
        .into_emulation();
    em.http2_options.as_ref().and_then(|o| o.headers_stream_dependency)
}

/// A measured priority in the frame's own spelling: weight 1..=256 becomes the wire's 0..=255.
fn as_frame(p: spec::H2Priority) -> wreq::http2::StreamDependency {
    wreq::http2::StreamDependency::new(
        wreq::http2::StreamId::from(p.dep_stream),
        (p.weight.clamp(1, 256) - 1) as u8,
        p.exclusive,
    )
}

/// A persona with no measured priority keeps the preset's, rather than inheriting another device's
/// measurement — the same fail-to-the-recorded-thing rule the rest of the bundle follows.
#[test]
fn a_bundle_that_measured_nothing_keeps_the_presets_priority() {
    let wire = spec::TlsProfile {
        id: "chrome_149".into(),
        platform: "linux".into(),
        h2_priority: None,
        quic: None,
        trust_anchors: None,
    };
    assert!(transport::check_wire(&wire).is_ok(), "an unmeasured wire must still be usable");
    assert!(
        preset_priority(&wire.id).is_some(),
        "the pinned preset must declare a priority, or 'keeps the preset's' means 'sends none'"
    );
}
