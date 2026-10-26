//! element.classList add/remove/toggle/contains.
use engine::Engine;
#[tokio::test]
async fn class_list() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><div id="d" class="a b"></div></body></html>"#).unwrap();
    e.eval("cl", r#"
        const d = document.getElementById("d");
        print("has=" + d.classList.contains("a") + " len=" + d.classList.length);
        d.classList.add("c", "a");
        print("added=" + d.className);
        d.classList.remove("b");
        print("removed=" + d.className);
        print("toggleOn=" + d.classList.toggle("x") + " -> " + d.className);
        print("toggleOff=" + d.classList.toggle("x") + " -> " + d.className);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "has=true len=2",
        "added=a b c",
        "removed=a c",
        "toggleOn=true -> a c x",
        "toggleOff=false -> a c",
    ]);
}
