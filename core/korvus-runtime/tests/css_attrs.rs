//! CSS.escape / CSS.supports + toggleAttribute + attributes NamedNodeMap.
use engine::Engine;
#[tokio::test]
async fn css_and_attrs() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><div id="d" class="a b" data-x="1"></div></body></html>"#).unwrap();
    e.eval("a", r#"
        print("escape=" + CSS.escape("a.b#c"));
        print("escapeDigit=" + CSS.escape("1abc"));
        print("supports=" + CSS.supports("color", "red") + "," + CSS.supports("color: red"));
        const d = document.getElementById("d");
        print("toggle1=" + d.toggleAttribute("hidden") + ",has=" + d.hasAttribute("hidden"));
        print("toggle2=" + d.toggleAttribute("hidden") + ",has=" + d.hasAttribute("hidden"));
        print("force=" + d.toggleAttribute("hidden", true) + "," + d.toggleAttribute("hidden", true));
        const at = d.attributes;
        print("attrs=" + at.length + ",id=" + at.getNamedItem("id").value + ",x=" + at["data-x"].value);
        print("node=" + d.getAttributeNode("class").value);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "escape=a\\.b\\#c",
        "escapeDigit=\\31 abc",
        "supports=true,true",
        "toggle1=true,has=true",
        "toggle2=false,has=false",
        "force=true,true",
        "attrs=4,id=d,x=1",
        "node=a b",
    ]);
}
