//! Pseudo-classes: first/last/only-child, checked, disabled, empty, not(), root, is().
use engine::Engine;
#[tokio::test]
async fn pseudo_classes() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r##"<!doctype html><html><body>
        <ul><li>A</li><li class="mid">B</li><li>C</li></ul>
        <input type="checkbox" checked><input type="text" disabled>
        <div></div><div>x</div></body></html>"##).unwrap();
    e.eval("p", r##"
        print("first=" + document.querySelector("li:first-child").textContent);
        print("last=" + document.querySelector("li:last-child").textContent);
        print("checked=" + document.querySelectorAll("input:checked").length);
        print("disabled=" + document.querySelectorAll("input:disabled").length);
        print("empty=" + document.querySelectorAll("div:empty").length);
        print("not=" + document.querySelectorAll("li:not(.mid)").length);
        print("root=" + (document.querySelector(":root").tagName));
    "##.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "first=A",
        "last=C",
        "checked=1",
        "disabled=1",
        "empty=1",
        "not=2",
        "root=HTML",
    ]);
}
