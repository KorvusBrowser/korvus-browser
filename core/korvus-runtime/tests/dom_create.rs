//! G1.9: createElement (DOM mutation) + canvas on a persona with NO GPU: getContext('webgl') is
//! null, and the 2D context still works.
//!
//! The persona is the shipped bundle without its device oracle. This ran on the default persona
//! after the test-persona sweep, and the shipped device HAS a GPU: `webgl` came back non-null, and
//! -- since a canvas holds one context -- the `2d` request after it was the null one, and
//! `fillRect` threw on it. The test was never about the shipped device.
use engine::Engine;
#[tokio::test]
async fn create_element_and_canvas_stub() {
    let mut e = Engine::new(persona::Overridden::new().without_device_oracle().boxed());
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("g19", r#"
        const div = document.createElement("DIV");
        print("tag=" + div.tagName);
        const c = document.createElement("canvas");
        print("webgl=" + (c.getContext("webgl") === null));
        print("webgl2=" + (c.getContext("webgl2") === null));
        const ctx = c.getContext("2d");
        print("has2d=" + (ctx !== null));
        ctx.fillRect(0, 0, 10, 10);  // must not throw
        print("wh=" + c.width + "x" + c.height);
        print("dataurl=" + c.toDataURL().startsWith("data:image/png"));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "tag=DIV",
        "webgl=true",
        "webgl2=true",
        "has2d=true",
        "wh=300x150",
        "dataurl=true",
    ]);
}
