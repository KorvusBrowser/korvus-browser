//! DOM navigation: children/parentElement/firstElementChild/nextElementSibling/childElementCount/innerHTML.
use engine::Engine;
#[tokio::test]
async fn dom_navigation() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r##"<!doctype html><html><body><ul id="l"><li>a</li><li>b</li><li>c</li></ul></body></html>"##).unwrap();
    e.eval("nav", r##"
        const ul = document.getElementById("l");
        print("count=" + ul.childElementCount);
        print("children=" + Array.from(ul.children).map(c => c.textContent).join(","));
        print("first=" + ul.firstElementChild.textContent);
        print("next=" + ul.firstElementChild.nextElementSibling.textContent);
        print("parentTag=" + ul.firstElementChild.parentElement.tagName);
        print("inner=" + ul.innerHTML);
    "##.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "count=3",
        "children=a,b,c",
        "first=a",
        "next=b",
        "parentTag=UL",
        "inner=<li>a</li><li>b</li><li>c</li>",
    ]);
}
