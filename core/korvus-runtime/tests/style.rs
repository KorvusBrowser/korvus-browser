//! element.style (inline CSS) + getBoundingClientRect.
//!
//! The rect used to be all zeros. It is now the real one: a `<div>` fills its containing block, so
//! it is as wide as `<body>` and sits at the top of it, with no height because it has no content
//! yet -- P3's inline flow is what gives text a height.
use engine::Engine;
use spec::Persona;

#[tokio::test]
async fn element_style() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><div id="d" style="color: red"></div></body></html>"#).unwrap();
    e.eval("s", r#"
        const d = document.getElementById("d");
        print("color=" + d.style.color);
        d.style.backgroundColor = "blue";
        print("attr=" + d.getAttribute("style"));
        print("get=" + d.style.getPropertyValue("background-color"));
        d.style.setProperty("margin", "0");
        print("css=" + d.style.cssText);
        const r = d.getBoundingClientRect();
        print("rect=" + r.width + "," + r.height + "," + r.top);
    "#.into()).unwrap();
    e.run().await.unwrap();
    // THE WIDTH IS DERIVED, NOT WRITTEN DOWN. A `<div>` fills its containing block, so its width is
    // the viewport minus the UA sheet's 8px body margin on each side -- and the viewport is the
    // PERSONA's screen. Hardcoding it (`1008`, from a 1024-wide persona) meant that switching the
    // shipped bundle broke a test about inline styles, which is about neither the bundle nor the
    // viewport. It failed on the Linux switch: 1920 - 16 = 1904.
    let viewport = persona::CapturedPersona::default().screen().width as f64;
    let expected_width = viewport - 16.0;
    // Chrome writes each declaration back as `prop: value;` (tools/realweb/seed/snl).
    assert_eq!(e.take_output(), vec![
        "color=red".to_string(),
        "attr=color: red; background-color: blue;".to_string(),
        "get=blue".to_string(),
        // A bare `0` length reads back with its unit (Chrome 150: `margin: 0` -> `0px`).
        "css=color: red; background-color: blue; margin: 0px;".to_string(),
        format!("rect={expected_width},0,8"),
    ]);
}
