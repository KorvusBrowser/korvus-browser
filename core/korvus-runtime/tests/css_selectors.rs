//! Compound + descendant + attribute CSS selectors.
use engine::Engine;
#[tokio::test]
async fn advanced_selectors() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r##"<!doctype html><html><body>
        <nav><a href="/1" class="btn primary">A</a></nav>
        <div class="card"><p class="lead">B</p><p>C</p></div>
        <input type="text" name="q"><input type="checkbox" name="c">
        <a class="btn">D</a></body></html>"##).unwrap();
    e.eval("s", r##"
        print("compound=" + document.querySelector("a.btn.primary").textContent);
        print("descendant=" + document.querySelector("div.card p.lead").textContent);
        print("attrPresence=" + document.querySelectorAll("a[href]").length);
        print("attrEq=" + document.querySelector("input[type=text]").name);
        print("attrStar=" + document.querySelectorAll("[name]").length);
        print("group=" + document.querySelectorAll("nav a, div.card p").length);
        print("navDesc=" + document.querySelector("nav a.btn").getAttribute("href"));
    "##.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "compound=A",
        "descendant=B",
        "attrPresence=1",
        "attrEq=q",
        "attrStar=2",
        "group=3",
        "navDesc=/1",
    ]);
}
