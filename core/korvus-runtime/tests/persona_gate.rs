//! The boot gate: an engine refuses to assemble an incoherent persona.
//!
//! Before this existed, the coherence audit ran in `korvus`'s `banner()` -- the no-arguments path --
//! so every command that actually loaded a page (`run`, `fetch`, `serve`, `probe`) audited nothing,
//! and the result was a printed line either way. A check whose only output is a line in a code path
//! nobody takes is not a check.
//!
//! These tests pin the two halves that make it one: a contradiction the persona has NOT accounted
//! for stops the engine, and a contradiction it HAS accounted for does not. Both matter. Without the
//! first the gate is decorative; without the second the gate would refuse to boot this tree at all,
//! because the shipped bundle is genuinely mixed and says so.

use spec::{ClientHints, NavigatorProfile, Persona, ScreenProfile, TlsProfile};

/// A persona assembled from parts that do not describe one device: a macOS UA over a Windows
/// platform, and a Chrome-151 identity over a Chrome-114 wire fingerprint. Every value is plausible
/// on its own, which is the point -- this is what a Frankenstein looks like from the inside.
struct Frankenstein {
    accounts_for: Vec<String>,
}

impl Persona for Frankenstein {
    fn navigator(&self) -> NavigatorProfile {
        NavigatorProfile {
            user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                         (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36"
                .into(),
            platform: "Win32".into(),
            hardware_concurrency: 8,
            // Non-empty, so the audit's "empty languages" finding does not mask the ones under test.
            languages: vec!["en-US".into()],
            ..Default::default()
        }
    }
    fn screen(&self) -> ScreenProfile {
        ScreenProfile::default()
    }
    fn client_hints(&self) -> ClientHints {
        ClientHints::default()
    }
    fn transport_profile(&self) -> TlsProfile {
        TlsProfile { id: "chrome_114".into(), platform: "macos".into(), ..Default::default() }
    }
    fn known_incoherences(&self) -> Vec<String> {
        self.accounts_for.clone()
    }
}

#[test]
fn a_persona_that_disagrees_with_itself_is_reported() {
    let f = Frankenstein { accounts_for: vec![] };
    let found = spec::coherence::unaccounted(&f);
    assert!(
        !found.is_empty(),
        "a Mac UA over a Win32 platform with a Chrome-114 wire fingerprint must not audit clean"
    );
    // The platform contradiction is the one no bundle should ever be allowed to declare away, so it
    // is named here rather than left to a count.
    assert!(
        found.iter().any(|i| i.layers.contains("platform")),
        "expected a platform disagreement, got: {:?}",
        found.iter().map(|i| i.layers).collect::<Vec<_>>()
    );
}

#[test]
fn declaring_a_contradiction_accounts_for_it_and_nothing_else() {
    let all = spec::coherence::unaccounted(&Frankenstein { accounts_for: vec![] });
    let ids: Vec<&str> = all.iter().map(spec::coherence::incoherence_id).collect();
    assert!(ids.contains(&"platform"), "ids: {ids:?}");

    // Declaring ONE id removes exactly that finding. The rest still stop the boot, which is what
    // keeps the allowlist from becoming a blanket.
    let narrowed = spec::coherence::unaccounted(&Frankenstein {
        accounts_for: vec!["platform".into()],
    });
    assert!(
        narrowed.iter().all(|i| spec::coherence::incoherence_id(i) != "platform"),
        "declaring `platform` should account for the platform finding"
    );
    assert!(
        narrowed.len() < all.len(),
        "declaring an id must remove a finding, not just be ignored"
    );
}

/// The persona this build actually ships must boot. It is mixed -- the manifest lists its
/// disagreements with reasons -- and every one of them is declared, so the gate lets it through.
/// If this ever fails, either a new incoherence appeared or a declaration was deleted without the
/// gap being closed; both are exactly the moment someone should have to look.
///
/// The host is SYNTHESISED from the bundle rather than measured, so this test asks "is the bundle
/// coherent on the machine it describes" -- a property of the repository. Measuring the real host
/// would have made it a property of whoever is running it, and it would fail on any box with a
/// different core count. Whether THIS machine matches is a separate question, asked below.
#[test]
fn the_shipped_persona_boots() {
    let p = persona::CapturedPersona::default();
    let unaccounted = spec::coherence::unaccounted_on(&p, &its_own_machine(&p));
    assert!(
        unaccounted.is_empty(),
        "the shipped bundle has an incoherence it does not account for: {:#?}\n\
         Add a [[known_incoherence]] to its manifest with a reason and a fix, or close the gap.",
        unaccounted
    );
}

/// The machine a persona claims to be running on, built from its own bundle.
fn its_own_machine(p: &persona::CapturedPersona) -> spec::coherence::HostFacts {
    use spec::Persona;
    spec::coherence::HostFacts {
        os: p.bundle().capture.os,
        cores: Some(p.navigator().hardware_concurrency),
        ..Default::default()
    }
}

/// The check that was missing entirely until now: a persona is audited against ITSELF and against
/// THE MACHINE UNDER IT, and only the first existed.
///
/// Everything a bundle says -- the GPU string, the screen, the font advances, the core count -- is a
/// statement about one computer. Compiled into a binary, those statements stay put while the
/// computer changes, and nothing said so. Moving this repo to another machine used to be silent.
#[test]
fn a_persona_on_the_wrong_machine_is_reported() {
    let p = persona::CapturedPersona::default();
    let elsewhere = spec::coherence::HostFacts {
        os: if p.bundle().capture.os == "macos" { "linux".into() } else { "macos".into() },
        cores: Some(3),
        ..Default::default()
    };
    let found = spec::coherence::audit_host(&p, &elsewhere);
    let ids: Vec<&str> = found.iter().map(spec::coherence::incoherence_id).collect();
    assert!(ids.contains(&"host-os"), "a foreign OS must be reported; got {ids:?}");
    assert!(ids.contains(&"host-cores"), "a different core count must be reported; got {ids:?}");

    // And it must be quiet on the machine the capture actually describes, or it is just noise.
    assert!(
        spec::coherence::audit_host(&p, &its_own_machine(&p)).is_empty(),
        "the host audit must not fire on the capture's own machine"
    );
}

/// A Linux host may honestly wear an Android persona -- same kernel, and the family check would
/// otherwise reject every mobile capture on the machine that produced it.
#[test]
fn android_over_linux_is_not_a_host_mismatch() {
    struct Phone;
    impl Persona for Phone {
        fn navigator(&self) -> NavigatorProfile {
            NavigatorProfile {
                user_agent: "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, \
                             like Gecko) Chrome/151.0.0.0 Mobile Safari/537.36"
                    .into(),
                platform: "Linux armv81".into(),
                hardware_concurrency: 8,
                languages: vec!["en-US".into()],
                ..Default::default()
            }
        }
        fn screen(&self) -> ScreenProfile {
            ScreenProfile::default()
        }
        fn client_hints(&self) -> ClientHints {
            ClientHints::default()
        }
        fn transport_profile(&self) -> TlsProfile {
            TlsProfile { id: "chrome_151".into(), platform: "android".into(), ..Default::default() }
        }
    }
    let host = spec::coherence::HostFacts { os: "linux".into(), cores: Some(8), ..Default::default() };
    let found = spec::coherence::audit_host(&Phone, &host);
    assert!(found.is_empty(), "android over a linux host is honest; got {found:?}");
}

/// Owner 2026-10-17 (a reference host-OS policy's map): a Linux host wears a Windows persona -- the commonest desktop, where Linux
/// desktop Chrome stands out -- and a Windows host a Windows one; a Mac persona still needs a Mac.
#[test]
fn a_linux_host_wears_a_windows_persona_and_not_a_mac_one() {
    struct Desktop(&'static str, &'static str);
    impl Persona for Desktop {
        fn navigator(&self) -> NavigatorProfile {
            NavigatorProfile {
                user_agent: format!("Mozilla/5.0 ({}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36", self.0),
                platform: self.1.into(),
                hardware_concurrency: 8,
                languages: vec!["en-US".into()],
                ..Default::default()
            }
        }
        fn screen(&self) -> ScreenProfile {
            ScreenProfile::default()
        }
        fn client_hints(&self) -> ClientHints {
            ClientHints::default()
        }
        fn transport_profile(&self) -> TlsProfile {
            TlsProfile { id: "chrome_149".into(), ..Default::default() }
        }
    }
    let windows = Desktop("Windows NT 10.0; Win64; x64", "Win32");
    let mac = Desktop("Macintosh; Intel Mac OS X 10_15_7", "MacIntel");
    let on = |os: &str| spec::coherence::HostFacts { os: os.into(), cores: Some(8), ..Default::default() };
    let ids = |p: &dyn Persona, os: &str| -> Vec<&'static str> {
        spec::coherence::audit_host(p, &on(os)).iter().map(spec::coherence::incoherence_id).collect()
    };
    assert!(ids(&windows, "linux").is_empty(), "a Windows persona on a Linux host is the map's");
    assert!(ids(&windows, "windows").is_empty());
    assert_eq!(ids(&mac, "linux"), vec!["host-os"], "a Mac persona on Linux is a host mismatch");
    assert_eq!(ids(&windows, "macos"), vec!["host-os"]);
}

/// The check this phase made real. `transport_profile()` used to be DERIVED from the UA's Chrome
/// major, so the audit compared the UA against a number computed from the UA -- a tautology sitting
/// in the audit as though it were evidence. The bundle declares the preset now, so the comparison
/// has two independent sides, and the first thing it says is true: we claim 151 and emit 137.
#[test]
fn the_wire_fingerprint_is_declared_not_derived_from_the_ua() {
    let p = persona::CapturedPersona::default();
    let ua_major: u32 = p
        .navigator()
        .user_agent
        .split("Chrome/")
        .nth(1)
        .and_then(|s| s.split('.').next())
        .and_then(|s| s.parse().ok())
        .expect("the shipped UA names a Chrome major");
    let b = p.bundle();
    assert_eq!(ua_major, b.capture.version, "the UA major and the capture agree");
    assert_eq!(
        p.transport_profile().id,
        b.transport.emulation,
        "the profile must come from the bundle manifest, not from the UA"
    );
    assert_ne!(
        p.transport_profile().id,
        format!("chrome_{ua_major}"),
        "a preset merely spelled from the UA major would make this whole test a tautology again"
    );
    // The platform is a second, independent axis in wreq-util 0.2 -- it steers the preset's own
    // sec-ch-ua-platform defaults -- so the bundle declares it too, from the same capture.
    //
    // Derived, not spelled. This line read `"macos"` until the shipped bundle became a Linux
    // capture, which is the shape of the bug the whole phase is about: a device fact written down
    // in a second place goes stale silently when the first place changes.
    assert_eq!(p.transport_profile().platform, b.capture.os);
    // The audit compares the WIRE it can see, not the preset's trailing integer (D110, coherence.rs): the shipped
    // bundle MEASURED its own trust_anchors / h2 / quic, so the `chrome_149` preset under a 152 UA is reconciled --
    // its ClientHello is JA4-identical to the real browser -- and the raw audit raises no `tls-version` finding from
    // the name. The gap between the UA and the preset NAME is still real and still what the assertions above check
    // (the profile comes from the manifest, not spelled from the UA); what changed is that the audit no longer cries
    // wolf over a wire it has measured. `an_unmeasured_preset_is_still_flagged` below is the negative control.
    let raw = spec::coherence::audit(&p);
    assert!(
        !raw.iter().any(|i| spec::coherence::incoherence_id(i) == "tls-version"),
        "the shipped persona's wire is measured, so the audit must not raise tls-version; findings: {:?}",
        raw.iter().map(|i| (i.layers, &i.detail)).collect::<Vec<_>>()
    );
}

/// The negative control for the wire-aware TLS check (D110): a persona on a bare old preset with NO measured wire is
/// still flagged `tls-version`, so suppressing the finding for the measured shipped bundle did not blind the audit.
#[test]
fn an_unmeasured_preset_is_still_flagged() {
    struct Bare(persona::CapturedPersona);
    impl spec::Persona for Bare {
        fn navigator(&self) -> spec::NavigatorProfile {
            self.0.navigator()
        }
        fn screen(&self) -> spec::ScreenProfile {
            self.0.screen()
        }
        fn transport_profile(&self) -> spec::TlsProfile {
            // An old preset, no trust_anchors / h2 / quic measured: the name is all there is, and it trails the UA.
            spec::TlsProfile { id: "chrome_120".into(), platform: "linux".into(), ..Default::default() }
        }
        fn navigator_extra(&self) -> spec::NavigatorExtra {
            self.0.navigator_extra()
        }
        fn client_hints(&self) -> spec::ClientHints {
            self.0.client_hints()
        }
        fn device_oracle_json(&self) -> String {
            self.0.device_oracle_json()
        }
    }
    let raw = spec::coherence::audit(&Bare(persona::CapturedPersona::default()));
    assert!(
        raw.iter().any(|i| spec::coherence::incoherence_id(i) == "tls-version"),
        "a bare old preset with no measured wire must still be flagged; findings: {:?}",
        raw.iter().map(|i| (i.layers, &i.detail)).collect::<Vec<_>>()
    );
}

/// A TLS profile the pinned wreq-util cannot emit must stop the process, not fall back. Falling back
/// means presenting some other browser's ClientHello under this persona's UA, which is the
/// incoherence the transport crate exists to prevent.
#[test]
fn an_unmappable_tls_profile_is_refused() {
    assert!(transport::emulation_for("chrome_137").is_ok());
    let err = transport::emulation_for("chrome_151").unwrap_err().to_string();
    assert!(err.contains("no TLS emulation preset"), "{err}");
    assert!(
        err.contains("Refusing to start"),
        "the error must say it refuses rather than substitutes: {err}"
    );
}

/// The engine's process environment must present the persona's identity, not the host's.
///
/// `navigator.language` and `Intl.DateTimeFormat().resolvedOptions().locale` are among the most
/// commonly compared pairs in a fingerprint, and they disagreed: the first came from the persona,
/// the second from ICU's default, which ICU reads from the environment. Nothing set it, so on this
/// machine a page saw `en-US` against `ko-KR`.
///
/// The first attempt at the fix respected a pre-set `LANG` the way the timezone code respects a
/// pre-set `TZ`, and so did nothing at all here -- this box has `LANG=ko_KR.UTF-8`. The asymmetry
/// is the point: a deployment matches TZ to its exit IP deliberately, while `LANG` is just whatever
/// the developer's desktop is. The persona wins, and `KORVUS_LOCALE` is the explicit override.
///
/// AND IT IS `intl.locale`, NOT `languages[0]`. The second attempt used the first
/// language, which is a DIFFERENT captured fact: real Brave 151 answers `"ko"` to a default
/// `Intl.DateTimeFormat()` while its `navigator.language` is `"ko-KR"`, and the edge bundle in this
/// tree records the pair the other way round. This test asserts the source the browsers actually
/// have, so a future "simplify it to languages[0]" fails here instead of on a live site.
// A tokio context is required: creating a `JsRuntime` posts delayed tasks, and V8 aborts the process
// if there is no runtime to honour them.
#[tokio::test]
async fn the_process_locale_and_timezone_come_from_the_persona() {
    let p = persona::CapturedPersona::default();
    // Building an engine is what applies them; the assertions are about the environment afterwards.
    let _e = engine::Engine::assemble(Box::new(persona::CapturedPersona::default()))
        .expect("the shipped persona assembles");

    let want_locale = p.client_hints().intl_locale;
    assert!(!want_locale.is_empty(), "the shipped bundle records an `intl.locale`");
    let lc = std::env::var("LC_ALL").unwrap_or_default();
    assert!(
        lc.starts_with(&want_locale),
        "ICU's locale must come from the persona's captured `intl.locale` ({want_locale}), got LC_ALL={lc:?}"
    );
    // The two facts are distinct, and this bundle is one of the captures where they differ -- so the
    // assertion above is not accidentally satisfied by the first language.
    let first_language = p.navigator().languages.first().cloned().expect("the bundle names a language");
    assert_ne!(
        want_locale, first_language,
        "this test only proves the SOURCE while the two differ in the shipped bundle; if a recapture \
         makes them equal, point it at a bundle where they do not"
    );

    let want_tz = p.client_hints().timezone;
    let tz = std::env::var("TZ").unwrap_or_default();
    assert_eq!(tz, want_tz, "the process timezone must be the persona's");
}

/// The refusal, end to end: `Engine::assemble` on a persona that describes a different machine
/// stops with a message that names the three ways out. Built from the real bundle with the core
/// count moved one past this machine's, so the mismatch is guaranteed wherever the test runs and
/// nothing else about the persona changes.
///
/// Run serially with the locale test above by virtue of being the only other test here that
/// touches the process environment -- `KORVUS_HOST` is read, never set, by this one.
#[tokio::test]
async fn a_persona_for_another_machine_refuses_to_assemble_and_says_why() {
    let here = spec::coherence::HostFacts::measure();
    let Some(cores) = here.cores else { return }; // a host that cannot count itself is not this test's subject
    let mut nav = persona::CapturedPersona::default().navigator();
    nav.hardware_concurrency = cores + 1;
    let p = persona::Overridden::new().navigator(nav).boxed();

    // The gate runs with `KORVUS_HOST=ignore` so it can boot cross-OS bundles and grade their served
    // values; `force_host_audit` makes the audit BAIL here regardless, so this test still proves the
    // refusal and its message. (A bare run with no ambient env is unaffected -- the audit bails anyway.)
    let err = match engine::force_host_audit(|| engine::Engine::assemble(p)) {
        Ok(_) => panic!("a persona claiming {} cores assembled on a {cores}-core machine", cores + 1),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("different machine"), "{err}");
    assert!(err.contains("persona<->host.cores"), "the finding must be named: {err}");
    for way_out in ["capture/host.py", "KORVUS_BUNDLE", "KORVUS_HOST=ignore"] {
        assert!(err.contains(way_out), "the message must offer `{way_out}`: {err}");
    }
    // And it must not have been mistaken for a self-contradiction, which has a different fix.
    assert!(!err.contains("[[known_incoherence]]"), "{err}");
}

// -- Which bundle a process wears -------------------------------------------------------------------
//
// The device facts are compiled in, which is what makes a build reproducible and what makes it wrong
// the moment the build runs somewhere else. `KORVUS_BUNDLE` is the door between those two: the
// embedded bundle stays the default, and a capture id picks another off disk at runtime.

/// No selection is the embedded bundle -- a build with nothing set behaves exactly as before.
#[test]
fn no_selection_is_the_shipped_bundle() {
    for empty in [None, Some("")] {
        let p = persona::CapturedPersona::select(empty).expect("the default always resolves");
        assert_eq!(p.navigator().user_agent, persona::CapturedPersona::shipped().navigator().user_agent);
    }
}

/// A name that does not resolve STOPS, rather than quietly falling back to the embedded bundle.
///
/// The fallback is the dangerous version: a typo in a deployment's environment would mean wearing a
/// device nobody chose, and every value would still look internally consistent, so nothing
/// downstream would ever notice.
#[test]
fn an_unresolvable_bundle_is_an_error_not_a_fallback() {
    let err = persona::CapturedPersona::select(Some("no-such-capture")).unwrap_err();
    assert!(err.contains("no-such-capture"), "the error must name what was asked for: {err}");
    assert!(err.contains("cannot read"), "{err}");

    // It is an id, not a path: nothing here should be able to read outside the bundles directory.
    for hostile in ["../../../etc", "a/b", ".."] {
        let err = persona::CapturedPersona::select(Some(hostile)).unwrap_err();
        assert!(err.contains("not a path"), "{hostile:?} should be refused as a path: {err}");
    }
}

/// Selecting the bundle this build ships gives the same persona the embedded copy does, and -- the
/// part that matters -- it carries its OWN manifest, so its declared incoherences and its declared
/// transport preset come from the bundle actually being worn.
#[test]
fn a_selected_bundle_uses_its_own_manifest() {
    let shipped = persona::CapturedPersona::default();
    let id = shipped.bundle().capture.id;
    let picked = persona::CapturedPersona::select(Some(&id)).expect("the shipped id resolves");

    assert_eq!(picked.navigator().user_agent, shipped.navigator().user_agent);
    assert_eq!(picked.transport_profile().id, shipped.transport_profile().id);
    assert_eq!(picked.known_incoherences(), shipped.known_incoherences());
    assert!(!picked.known_incoherences().is_empty(), "a manifest read from disk, not an empty list");
    assert_eq!(picked.bundle().capture.id, id);
}
