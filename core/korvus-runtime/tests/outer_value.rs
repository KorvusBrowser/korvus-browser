//! element.outerHTML + value/name/href reflected properties.
use engine::Engine;
#[tokio::test]
async fn outer_and_value() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><input id="i" name="user" value="bob"><a id="a" href="/x">L</a></body></html>"#).unwrap();
    e.eval("o", r#"
        const i = document.getElementById("i");
        print("value=" + i.value + " name=" + i.name);
        i.value = "alice";
        print("newVal=" + i.getAttribute("value"));
        print("outer=" + i.outerHTML);
        const a = document.getElementById("a");
        print("href=" + a.href);
    "#.into()).unwrap();
    e.run().await.unwrap();
    // `input.value = "alice"` sets the API VALUE and its dirty flag; the `value` CONTENT attribute
    // (and so `outerHTML` and `defaultValue`) keeps what the markup said. Asked of stock Chrome
    // 150.0.7871.46 with this exact script: `newVal=bob`, `value="bob"` in `outerHTML`.
    // The expectation used to be `alice`, from an engine that wrote the attribute.
    assert_eq!(e.take_output(), vec![
        "value=bob name=user",
        "newVal=bob",
        "outer=<input id=\"i\" name=\"user\" value=\"bob\">",
        "href=/x",
    ]);
}
