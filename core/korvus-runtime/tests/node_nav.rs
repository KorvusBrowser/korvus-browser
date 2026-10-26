//! childNodes/firstChild/lastChild/nextSibling (all node types) + nodeName/nodeValue.
use engine::Engine;
#[tokio::test]
async fn node_navigation() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><p id="p">Hi <b>there</b> world</p></body></html>"#).unwrap();
    e.eval("n", r#"
        const p = document.getElementById("p");
        print("childNodes=" + p.childNodes.length);
        print("nodeNames=" + Array.from(p.childNodes).map(n => n.nodeName).join(","));
        print("firstVal=" + JSON.stringify(p.firstChild.nodeValue));
        print("lastVal=" + JSON.stringify(p.lastChild.nodeValue));
        const b = p.querySelector("b");
        print("bName=" + b.nodeName + " bVal=" + JSON.stringify(b.nodeValue));
        print("prev=" + JSON.stringify(b.previousSibling.nodeValue) + " next=" + JSON.stringify(b.nextSibling.nodeValue));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "childNodes=3",
        "nodeNames=#text,B,#text",
        "firstVal=\"Hi \"",
        "lastVal=\" world\"",
        "bName=B bVal=null",
        "prev=\"Hi \" next=\" world\"",
    ]);
}
