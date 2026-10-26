//! isEqualNode / isSameNode / hasChildNodes / getRootNode / compareDocumentPosition.
use engine::Engine;
#[tokio::test]
async fn node_comparison() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><div id="a"><span class="x">hi</span></div><div id="b"><span class="x">hi</span></div></body></html>"#).unwrap();
    e.eval("a", r#"
        const a = document.getElementById("a");
        const b = document.getElementById("b");
        const sa = a.querySelector("span");
        const sb = b.querySelector("span");
        print("same=" + a.isSameNode(a) + "," + a.isSameNode(b));
        print("equal=" + sa.isEqualNode(sb));
        sa.setAttribute("data-z", "1");
        print("equal2=" + sa.isEqualNode(sb));
        const c = sa;
        print("hasKids=" + a.hasChildNodes() + "," + c.hasChildNodes());
        print("root=" + (a.getRootNode().nodeType === 9 || a.getRootNode().tagName === "HTML" || !!a.getRootNode()));
        const span = a.querySelector("span");
        const pos = a.compareDocumentPosition(span);
        print("contains=" + ((pos & 16) !== 0) + "," + ((pos & 4) !== 0));
        const pos2 = span.compareDocumentPosition(a);
        print("containedby=" + ((pos2 & 8) !== 0) + "," + ((pos2 & 2) !== 0));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "same=true,false",
        "equal=true",
        "equal2=false",
        "hasKids=true,true",
        "root=true",
        "contains=true,true",
        "containedby=true,true",
    ]);
}
