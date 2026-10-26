//! removeChild/remove + Node constants + nodeType.
use engine::Engine;
#[tokio::test]
async fn remove_and_node_consts() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><ul id="l"><li>a</li><li>b</li><li>c</li></ul></body></html>"#).unwrap();
    e.eval("r", r#"
        const ul = document.getElementById("l");
        print("before=" + ul.childElementCount);
        ul.removeChild(ul.firstElementChild);
        print("afterRemoveChild=" + ul.childElementCount + " first=" + ul.firstElementChild.textContent);
        ul.firstElementChild.remove();
        print("afterRemove=" + ul.childElementCount + " first=" + ul.firstElementChild.textContent);
        print("nodeType=" + ul.nodeType + " ELEMENT=" + Node.ELEMENT_NODE + " TEXT=" + Node.TEXT_NODE);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "before=3",
        "afterRemoveChild=2 first=b",
        "afterRemove=1 first=c",
        "nodeType=1 ELEMENT=1 TEXT=3",
    ]);
}
