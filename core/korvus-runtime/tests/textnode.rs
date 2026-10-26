//! Node.normalize + Text.splitText + nodeValue/data setters.
use engine::Engine;
#[tokio::test]
async fn text_operations() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><p id="p">Hello</p></body></html>"#).unwrap();
    e.eval("a", r#"
        const p = document.getElementById("p");
        const t = p.firstChild;
        print("data=" + t.data);
        const rest = t.splitText(2);
        print("split=" + t.data + "|" + rest.data);
        print("children=" + p.childNodes.length);
        p.normalize();
        print("normalized=" + p.childNodes.length + ":" + p.firstChild.data);
        p.firstChild.nodeValue = "World";
        print("set=" + p.textContent);
        p.firstChild.data = "Again";
        print("data2=" + p.textContent);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "data=Hello",
        "split=He|llo",
        "children=2",
        "normalized=1:Hello",
        "set=World",
        "data2=Again",
    ]);
}
