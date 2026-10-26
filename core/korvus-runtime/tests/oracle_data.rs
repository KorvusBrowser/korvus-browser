//! GS.4 (device-oracle, data-integrity slice): the persona's recorded device oracle (WebGL
//! renderer/params, canvas image, the audio device's sample rate) reaches JS intact through the
//! **observable** native surface -- `canvas.getContext('webgl')`, `canvas.toDataURL()`,
//! `AudioContext` -- not through any page-visible internal op. (The raw `op_persona_oracle` is
//! internal: `Deno` is removed from the page world, so it must not be reachable from script.)
//!
//! R3.5 changed the audio line, and it had to. It read
//! `new OfflineAudioContext(1, 44100, 44100).createDynamicsCompressor().reduction ===
//! -19.004104614257812`: one recorded constant that every compressor reported for every graph,
//! every threshold and every input, reported here for a compressor that had never processed a
//! sample. `reduction` is computed now, so an unrun compressor correctly reads 0 and that assertion
//! is no longer about the oracle at all. What the oracle still carries for audio is
//! `AudioContext.sampleRate` -- the live device's output rate, the one device-varying number in the
//! family -- so that is what this test reads.
use engine::Engine;
use spec::Persona;

#[tokio::test]
async fn persona_oracle_reaches_js() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("o", r#"
        // Deno must be gone from the page world (a `typeof Deno` tell).
        print("noDeno=" + (typeof Deno === "undefined"));
        const c = document.createElement("canvas");
        const gl = c.getContext("webgl");
        const dbg = gl.getExtension("WEBGL_debug_renderer_info");
        print("renderer=" + gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL));
        print("maxTex=" + gl.getParameter(gl.MAX_TEXTURE_SIZE));
        print("canvas=" + c.toDataURL().startsWith("data:image/png;base64,iVBOR"));
        print("audio=" + new AudioContext().sampleRate);
    "#.into()).unwrap();
    e.run().await.unwrap();
    // DERIVED FROM THE BUNDLE, not written down. These three literals were the mac capture's
    // (`Apple M3 Pro`, 16384, 48000), so switching the shipped bundle turned a test about whether
    // the oracle REACHES JS into a test about which device it happens to describe. What this file is
    // for is the path, not the payload.
    //
    // `take_output` DRAINS, so it is called once. Calling it twice -- as the first version of this
    // did, to peek at the audio line -- leaves the assertion comparing against an empty vec.
    let out = e.take_output();
    let p = persona::CapturedPersona::default();
    let oracle: serde_json::Value =
        serde_json::from_str(&p.device_oracle_json()).expect("the oracle block parses");
    // the WebGL values a page reads are the device's where this PC has one
    let webgl = crate::webgl_device::page_webgl(&p.device_oracle_json());
    let want_renderer = webgl["unmaskedRenderer"].as_str().unwrap_or_default().to_string();
    let want_max_tex = webgl["paramsByNum"]["3379"].as_i64().expect("MAX_TEXTURE_SIZE (0xD33)");

    assert_eq!(out.len(), 5, "expected five printed lines, got {out:?}");
    assert_eq!(out[0], "noDeno=true");
    assert_eq!(out[1], format!("renderer={want_renderer}"));
    assert_eq!(out[2], format!("maxTex={want_max_tex}"));
    assert_eq!(out[3], "canvas=true");

    // `AudioContext.sampleRate` is the live device's output rate, and the offline context the render
    // probe uses cannot report it -- it renders at whatever rate it was constructed with. So nothing
    // captured it and the engine answers a hardcoded 48000: a persona-visible value that comes from
    // no persona. The collector records it from captureVersion 2 on; a bundle without it says so
    // here rather than asserting a number nobody recorded.
    match oracle["audio"]["sampleRate"].as_i64() {
        Some(r) => assert_eq!(out[4], format!("audio={r}")),
        None => eprintln!(
            "note: {} records no AudioContext.sampleRate, so the engine's {} is unchecked. \
             Re-capture with the current collector to close it.",
            p.bundle().capture.id, out[4]
        ),
    }
}

/// A persona with no captured device answers "no WebGL" rather than fabricating one.
///
/// This used `NullPersona`, which is gone: an empty persona is not a device with no GPU, it is a
/// browser with no user-agent, no platform and no wire fingerprint, and the boot gate refuses to
/// start it. What the test is actually about is the ORACLE being empty, so that is what it varies --
/// everything else stays the shipped device.
#[tokio::test]
async fn a_persona_with_no_oracle_has_no_webgl() {
    let mut e = Engine::new(persona::Overridden::new().without_device_oracle().boxed());
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("o", r#"
        const c = document.createElement("canvas");
        print("webgl=" + (c.getContext("webgl") === null) + " noDeno=" + (typeof Deno === "undefined"));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec!["webgl=true noDeno=true"]);
}
