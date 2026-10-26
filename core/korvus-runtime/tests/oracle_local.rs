//! GS.6 local oracle (DoD-7 scaffold, no network). A page-load fingerprint probe collects korvus's LIVE
//! surface (automation tells, UA/platform/UA-CH coherence, plugins, WebGL) and renders a human/bot
//! verdict -- the SAME shape a real detector applies. The point is the NEGATIVE CONTROL: the CapturedPersona
//! is classified "human", and a deliberately-mixed persona (Mac UA + Windows platform) is classified
//! "bot" by the SAME probe. A control that is NOT flagged would mean the probe proves nothing.
//!
//! This is a self-authored probe (so it only catches tells we know), NOT a substitute for the real
//! GS.6 oracle (CreepJS / a public bot-test), which needs network. It IS a
//! runnable regression that enforces the negative-control discipline.
use engine::Engine;
use spec::{ClientHints, NavigatorExtra, NavigatorProfile, Persona, ScreenProfile, TlsProfile};

const PROBE: &str = r#"
    // A minimal detector: any failed check => "bot". Mirrors what typical fingerprinters read.
    const fails = [];
    if (navigator.webdriver !== false) fails.push("webdriver");
    const cdc = Object.getOwnPropertyNames(globalThis).concat(
        typeof document !== "undefined" ? Object.getOwnPropertyNames(document) : []
    ).some(k => k.indexOf("cdc_") === 0 || k.indexOf("$cdc_") === 0);
    if (cdc) fails.push("cdc");
    const ua = navigator.userAgent || "";
    if (!/Chrome\/\d+/.test(ua)) fails.push("ua");
    // UA os-family <-> navigator.platform <-> UA-CH platform coherence, for EVERY family. This
    // checked Macintosh only, so a Linux persona could have worn Win32 and passed the probe.
    const fam = /Macintosh/.test(ua) ? ["MacIntel", "macOS"]
              : /Windows/.test(ua)   ? ["Win32", "Windows"]
              : /Android/.test(ua)   ? [/^Linux (armv|aarch64)/, "Android"]
              : /Linux/.test(ua)     ? [/^Linux (x86_64|i686)/, "Linux"]
              : null;
    if (fam) {
        const okPlat = fam[0] instanceof RegExp ? fam[0].test(navigator.platform) : navigator.platform === fam[0];
        if (!okPlat) fails.push("platform");
    }
    const uad = navigator.userAgentData;
    if (fam && uad && uad.platform !== fam[1]) fails.push("uach-platform");
    // brand major must match the UA major.
    const m = ua.match(/Chrome\/(\d+)/);
    if (uad && m) {
        const bm = (uad.brands.find(b => /Chrome|Chromium/.test(b.brand)) || {}).version;
        if (bm && bm !== m[1]) fails.push("brands");
    }
    if (!navigator.plugins || navigator.plugins.length === 0) fails.push("plugins");
    // WebGL present with a real GPU renderer.
    try {
        const c = document.createElement("canvas");
        const gl = c.getContext("webgl");
        if (!gl) { fails.push("webgl-null"); }
        else {
            const dbg = gl.getExtension("WEBGL_debug_renderer_info");
            const r = dbg ? gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL) : "";
            if (!r) fails.push("webgl-renderer");
        }
    } catch (e) { fails.push("webgl-throw"); }
    print("verdict=" + (fails.length === 0 ? "human" : "bot") + " fails=" + JSON.stringify(fails));
"#;

async fn verdict(persona: Box<dyn Persona>) -> String {
    let mut e = Engine::new(persona);
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("probe", PROBE.into()).unwrap();
    e.run().await.unwrap();
    e.take_output().join("\n")
}

#[tokio::test]
async fn captured_persona_classified_human() {
    let v = verdict(Box::new(persona::CapturedPersona::default())).await;
    assert_eq!(v, "verdict=human fails=[]", "captured persona must pass the probe");
}

/// The negative control: a Frankenstein persona (the shipped UA over a Windows platform + UA-CH)
/// MUST be flagged, or the probe is meaningless.
///
/// It is flagged EARLIER now than this test was written for. The boot gate audits the persona
/// before there is a JS runtime to probe, and a platform that contradicts the UA is one of the
/// disagreements no bundle may declare away -- so `Engine::assemble` refuses, naming the platform.
/// That is a stronger control than a probe (it fires whether or not any page asks), and it is
/// asserted first. The probe's own platform rule is then checked in isolation on the same
/// Frankenstein values, so a later loosening of the probe cannot hide behind the gate.
#[tokio::test]
async fn frankenstein_persona_classified_bot() {
    struct Franken(persona::CapturedPersona);
    impl Persona for Franken {
        fn navigator(&self) -> NavigatorProfile {
            let mut n = self.0.navigator();
            n.platform = "Win32".into();
            n
        }
        fn screen(&self) -> ScreenProfile { self.0.screen() }
        fn transport_profile(&self) -> TlsProfile { self.0.transport_profile() }
        fn navigator_extra(&self) -> NavigatorExtra { self.0.navigator_extra() }
        fn client_hints(&self) -> ClientHints {
            let mut h = self.0.client_hints();
            h.platform = "Windows".into();
            h
        }
        fn device_oracle_json(&self) -> String { self.0.device_oracle_json() }
        fn known_incoherences(&self) -> Vec<String> { self.0.known_incoherences() }
    }
    let err = match Engine::assemble(Box::new(Franken(persona::CapturedPersona::default()))) {
        Ok(_) => panic!("a UA/platform contradiction must not assemble"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("platform"), "the gate must name the platform disagreement: {err}");

    // The probe itself, on a coherent engine fed the Frankenstein's values by hand: the rule that
    // would have caught it had the gate not.
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("franken", format!(r#"
        const navigator = {{
            webdriver: false,
            userAgent: {ua:?},
            platform: "Win32",
            userAgentData: {{ platform: "Windows", brands: [] }},
            plugins: [{{}}],
        }};
        {probe}
    "#, ua = persona::CapturedPersona::default().navigator().user_agent, probe = PROBE)).unwrap();
    e.run().await.unwrap();
    let v = e.take_output().join("\n");
    assert!(v.starts_with("verdict=bot"), "the probe must flag the control too, got: {v}");
    assert!(v.contains("platform"), "the probe should fail the platform-coherence check, got: {v}");
}
