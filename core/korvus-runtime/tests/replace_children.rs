//! Element.replaceChildren.
use engine::Engine;
#[tokio::test]
async fn replace_children() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><ul id="l"><li>old1</li><li>old2</li></ul></body></html>"#).unwrap();
    e.eval("a", r#"
        const ul = document.getElementById("l");
        const a = document.createElement("li"); a.append("new1");
        ul.replaceChildren(a, "text", document.createElement("li"));
        print("count=" + ul.childNodes.length);
        print("first=" + ul.firstChild.textContent);
        print("hasOld=" + ul.textContent.includes("old"));
        ul.replaceChildren();
        print("empty=" + ul.childNodes.length);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "count=3",
        "first=new1",
        "hasOld=false",
        "empty=0",
    ]);
}
