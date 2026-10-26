//! G1.2 thesis + G1.3 host surface: native `navigator`/`screen` whose getters are `[native code]`
//! and read the persona live; plus `window`/`document`/`console` shells.

use engine::Engine;
use spec::{Persona, ScreenProfile};

// The values a page reads are the SHIPPED bundle's, with one screen overridden -- so the
// expectations come from the bundle too. These two tests spelled out `KorvusUA/1.0`, `MacIntel`
// and `hc=10`: a test persona that no longer exists, and a Mac capture that has since been
// replaced. What they prove is that the getters are native and read the persona LIVE; which
// persona is not the point, and writing one down here is how they went red.
fn shipped() -> persona::CapturedPersona {
    persona::CapturedPersona::default()
}
/// The thesis proof: navigator.userAgent's getter is a genuine native function.
#[tokio::test]
async fn navigator_getter_is_native() {
    let mut e = Engine::new(persona::Overridden::new().screen(ScreenProfile { width: 1920, height: 1080, color_depth: 24, device_pixel_ratio: 2.0, ..Default::default() }).boxed());
    e.run().await.expect("init loop");
    e.eval(
        "probe",
        r#"
        const d = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(navigator), "userAgent");
        print("isNative=" + d.get.toString().includes("[native code]"));
        print("value=" + navigator.userAgent);
        "#
        .into(),
    )
    .expect("probe");
    e.run().await.expect("loop");
    assert_eq!(e.take_output(), vec!["isNative=true".to_string(), format!("value={}", shipped().navigator().user_agent)]);
}

/// The G1.3 host surface reads persona values and exposes window/document/console.
#[tokio::test]
async fn host_surface() {
    let mut e = Engine::new(persona::Overridden::new().screen(ScreenProfile { width: 1920, height: 1080, color_depth: 24, device_pixel_ratio: 2.0, ..Default::default() }).boxed());
    e.run().await.expect("init loop");
    e.eval(
        "surface",
        r#"
        print("ua=" + navigator.userAgent);
        print("platform=" + navigator.platform);
        print("hc=" + navigator.hardwareConcurrency);
        print("dm=" + navigator.deviceMemory);
        print("screen=" + screen.width + "x" + screen.height + "@" + screen.colorDepth);
        print("window=" + (window === globalThis));
        print("doc=" + (typeof document));
        console.log("console", "works");
        const et = new EventTarget();
        et.addEventListener("ping", (e) => print("event=" + e.type));
        et.dispatchEvent(new Event("ping"));
        "#
        .into(),
    )
    .expect("surface");
    e.run().await.expect("loop");
    let nav = shipped().navigator();
    assert_eq!(
        e.take_output(),
        vec![
            format!("ua={}", nav.user_agent),
            format!("platform={}", nav.platform),
            format!("hc={}", nav.hardware_concurrency),
            format!("dm={}", nav.device_memory),
            "screen=1920x1080@24".to_string(),
            "window=true".to_string(),
            "doc=object".to_string(),
            "console works".to_string(),
            "event=ping".to_string(),
        ]
    );
}
