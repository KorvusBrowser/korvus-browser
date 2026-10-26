//! A Windows persona on this host (owner 2026-10-17, a reference host-OS policy's map: a Linux host wears a Windows persona).
//!
//! The capture of Windows Chrome 152.0.7977.83 (`plugins/korvus-persona/bundles/chrome-152-windows-20261017T0248`, the
//! owner's Windows PC) worn the way `persona::from_env` wears every persona: with THIS machine's hardware -- its
//! cores and memory as Chrome counts them, its GPU named as Windows Chrome names it (owner 2026-10-12: never another
//! machine's) -- and every other value the capture's, the D3D11 device's WebGL limits and extensions included.

use engine::Engine;

fn windows() -> persona::CapturedPersona {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/korvus-persona/bundles/chrome-152-windows-20261017T0248");
    persona::CapturedPersona::from_dir(&dir).expect("the Windows capture loads")
}

fn recorded_webgl() -> serde_json::Value {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/korvus-persona/bundles/chrome-152-windows-20261017T0248/values.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir).unwrap()).unwrap();
    v["deviceOracle"]["webgl"].clone()
}

async fn page() -> Engine {
    let host = spec::coherence::HostFacts::measure();
    let mut e = Engine::new(persona::Overridden::on(windows()).hardware_of(&host).boxed());
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><canvas id="c" width="8" height="8"></canvas></body></html>"#).unwrap();
    e.run().await.unwrap();
    e
}

fn eval(e: &mut Engine, expr: &str) -> serde_json::Value {
    let src = format!("(() => {{ try {{ return JSON.stringify({expr}); }} catch (err) {{ return 'throw:' + err; }} }})()");
    let s = e.eval_value("windows-persona", src).unwrap().as_str().unwrap().to_string();
    serde_json::from_str(&s).unwrap_or_else(|_| panic!("{expr}: {s}"))
}

#[test]
fn a_linux_host_wears_it_without_a_host_mismatch() {
    let host = spec::coherence::HostFacts::measure();
    let p = persona::Overridden::on(windows()).hardware_of(&host);
    let found = spec::coherence::audit_host(&p, &host);
    assert!(found.is_empty(), "the Windows persona on this {} host: {found:?}", host.os);
}

#[tokio::test]
async fn the_page_sees_windows_and_this_machines_hardware() {
    let host = spec::coherence::HostFacts::measure();
    let mut e = page().await;
    let got = eval(&mut e, "[navigator.platform, navigator.userAgentData.platform, navigator.hardwareConcurrency, navigator.deviceMemory]");
    assert_eq!(got[0], "Win32");
    assert_eq!(got[1], "Windows");
    if let Some(cores) = host.cores {
        assert_eq!(got[2], cores, "this machine's cores, not the capture machine's 24");
    }
    if let Some(mib) = host.memory_mib {
        assert_eq!(got[3].as_f64(), Some(spec::coherence::chrome_device_memory(mib)));
    }
}

#[tokio::test]
async fn the_fonts_are_a_default_windows_installations() {
    let mut e = page().await;
    // the standard font of a Korean Windows Chrome, the default of every document
    assert_eq!(eval(&mut e, "getComputedStyle(document.body).fontFamily"), "\"Malgun Gothic\"");
    // a default font measures as itself; Office's Arial Narrow, which the capturing PC had, is absent: the fallback
    let w = |f: &str| format!(r#"(() => {{ const c = document.createElement("canvas").getContext("2d"); c.font = '100px {f}, nosuchfont'; return c.measureText("mmmmmmmmmmlli").width; }})()"#);
    let segoe = eval(&mut e, &w("\"Segoe UI\""));
    let narrow = eval(&mut e, &w("\"Arial Narrow\""));
    let none = eval(&mut e, &w("nosuchfont2"));
    assert_ne!(segoe, none, "Segoe UI is installed");
    assert_eq!(narrow, none, "Arial Narrow is not a default font");
}

#[tokio::test]
async fn webgl_is_the_d3d11_device_naming_this_machines_gpu() {
    let mut e = page().await;
    let gl = r#"document.getElementById("c").getContext("webgl")"#;
    if eval(&mut e, &format!("{gl} === null")) == true {
        return; // no usable GPU here: Chrome has no WebGL then either
    }
    let rec = recorded_webgl();
    let renderer = eval(&mut e, &format!(r#"({gl}.getExtension("WEBGL_debug_renderer_info"), {gl}.getParameter(37446))"#));
    let renderer = renderer.as_str().unwrap().to_string();
    assert!(
        renderer.starts_with("ANGLE (") && renderer.ends_with(") Direct3D11 vs_5_0 ps_5_0, D3D11)"),
        "Windows Chrome's ANGLE D3D11 string: {renderer}"
    );
    assert_ne!(renderer, rec["unmaskedRenderer"].as_str().unwrap(), "this machine's GPU, not the capture machine's");
    let vendor = eval(&mut e, &format!("{gl}.getParameter(37445)"));
    assert!(vendor.as_str().unwrap().starts_with("Google Inc. ("), "{vendor}");
    // the limits and the extension list are the D3D11 device's, as recorded
    assert_eq!(eval(&mut e, &format!("{gl}.getParameter({gl}.MAX_TEXTURE_SIZE)")), rec["paramsByNum"]["3379"]);
    assert_eq!(eval(&mut e, &format!("{gl}.getSupportedExtensions()")), rec["extensions"]);
    let gl2 = r#"document.createElement("canvas").getContext("webgl2")"#;
    assert_eq!(eval(&mut e, &format!("{gl2}.getSupportedExtensions()")), rec["extensionsV2"]);
    // OVR_multiview2, which only the D3D11 device has: its limit, once the extension is enabled
    assert_eq!(
        eval(&mut e, &format!(r#"(() => {{ const g = {gl2}; const x = g.getExtension("OVR_multiview2"); return x && g.getParameter(x.MAX_VIEWS_OVR); }})()"#)),
        rec["paramsByNumV2"]["38449"]
    );
}
