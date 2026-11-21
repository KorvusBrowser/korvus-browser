//! A trusted mouse or keyboard event carries an `InputDeviceCapabilities` in `sourceCapabilities`
//! (`firesTouchEvents` false), as Chrome does; its pointer and wheel events, and any script-made event, carry null.
//!
//! Measured on headed Google Chrome 157, the same CDP input sequence `drive.py` fires: `mousemove`/`mousedown`/
//! `mouseup`, `click`/`dblclick`/`auxclick`/`contextmenu`, `keydown`/`keyup` and `focus`/`blur` read
//! `sourceCapabilities.firesTouchEvents === false` with `constructor.name === "InputDeviceCapabilities"`, while
//! `pointermove`/`pointerdown`/`pointerup`, `wheel` and `beforeinput` read `sourceCapabilities === null`. Every event
//! this engine made read null, so `mouseEvent.sourceCapabilities.firesTouchEvents` threw where Chrome answers `false`
//! -- an automation tell a behavioural sensor reads.

use engine::Engine;
use webapi::input::{held_button, Modifiers, MouseKind};

const PAGE: &str = r#"<!doctype html><html><head><style>body{margin:0;width:3000px;height:3000px}</style></head>
<body tabindex="0"><div id="root">source-capabilities fixture</div><input id="f">
<script>
globalThis.__sc = {};
const rec = e => { if (globalThis.__sc[e.type]) return;
  const s = e.sourceCapabilities;
  globalThis.__sc[e.type] = s === null ? "null" : (s === undefined ? "undefined"
    : {fte: s.firesTouchEvents, ctor: s.constructor.name}); };
for (const t of ["mousemove", "pointermove", "wheel", "keydown"])
  window.addEventListener(t, rec, {capture: true});
// a script-constructed (untrusted) event must keep null sourceCapabilities
globalThis.__scripted = (new MouseEvent("mousemove")).sourceCapabilities === null ? "null" : "set";
</script></body></html>"#;

/// Drive one move, one wheel notch and one key, then read what each event's `sourceCapabilities` was.
async fn caps() -> (serde_json::Value, String) {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("engine boot");
    e.set_url("http://127.0.0.1:9/");
    e.load_html_injected(PAGE, &[]).expect("load page");
    e.run().await.expect("run page");
    e.mouse_event(MouseKind::Move, 120.0, 120.0, held_button(0), 0, 0, Modifiers::default()).expect("move");
    e.run().await.expect("run move");
    e.mouse_wheel(120.0, 120.0, 0.0, 120.0, 0, Modifiers::default()).expect("wheel");
    e.run().await.expect("run wheel");
    // a key into a focused input (a bare dispatch_key with nothing focused dispatches nothing): fires a trusted keydown
    e.type_into("#f", "a").await.expect("type");
    e.run().await.expect("run key");
    e.eval("sc", "print(JSON.stringify({sc: globalThis.__sc, scripted: globalThis.__scripted}))".to_string()).expect("eval");
    e.run().await.expect("run eval");
    let out = e.take_output();
    let line = out.iter().rev().find(|l| l.starts_with('{')).unwrap_or_else(|| panic!("no output: {out:?}"));
    let v: serde_json::Value = serde_json::from_str(line).expect("json");
    (v["sc"].clone(), v["scripted"].as_str().unwrap_or("?").to_string())
}

#[tokio::test]
async fn trusted_mouse_and_key_carry_input_device_capabilities() {
    let (sc, scripted) = caps().await;
    // a trusted mouse and key event: a real InputDeviceCapabilities whose firesTouchEvents is false
    for t in ["mousemove", "keydown"] {
        assert_eq!(sc[t]["ctor"], "InputDeviceCapabilities", "{t}: sourceCapabilities.constructor.name -- {sc}");
        assert_eq!(sc[t]["fte"], serde_json::json!(false), "{t}: sourceCapabilities.firesTouchEvents -- {sc}");
    }
    // a pointer and a wheel event carry null, as Chrome
    assert_eq!(sc["pointermove"], "null", "pointermove sourceCapabilities -- {sc}");
    assert_eq!(sc["wheel"], "null", "wheel sourceCapabilities -- {sc}");
    // a script-constructed event is untrusted and keeps null
    assert_eq!(scripted, "null", "a scripted MouseEvent keeps null sourceCapabilities");
}
