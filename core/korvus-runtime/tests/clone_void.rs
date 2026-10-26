//! void-element serialization + cloneNode.
use engine::Engine;
#[tokio::test]
async fn clone_and_void() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><div id="d"><input value="x"><br><span>hi</span></div></body></html>"#).unwrap();
    e.eval("cv", r#"
        const d = document.getElementById("d");
        print("void=" + d.innerHTML);
        const clone = d.cloneNode(true);
        print("clone=" + clone.outerHTML);
        print("shallow=" + d.cloneNode(false).childElementCount);
        // clone is detached
        print("detached=" + (clone.parentElement === null));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "void=<input value=\"x\"><br><span>hi</span>",
        "clone=<div id=\"d\"><input value=\"x\"><br><span>hi</span></div>",
        "shallow=0",
        "detached=true",
    ]);
}
