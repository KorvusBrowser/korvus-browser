//! DOM mutation: setAttribute/removeAttribute, textContent/id/className/title setters, appendChild.
use engine::Engine;
#[tokio::test]
async fn dom_mutation() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><head><title>Orig</title></head><body><div id="a">x</div></body></html>"#).unwrap();
    e.eval("mut", r#"
        document.title = "NewTitle";
        print("title=" + document.title);
        const a = document.getElementById("a");
        a.setAttribute("data-k", "v");
        print("attr=" + a.getAttribute("data-k"));
        a.textContent = "replaced";
        print("text=" + a.textContent);
        a.className = "cls";
        print("class=" + a.className);
        a.removeAttribute("data-k");
        print("removed=" + (a.getAttribute("data-k") === null));
        const child = document.createElement("span");
        child.textContent = "kid";
        a.appendChild(child);
        print("after=" + a.textContent);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "title=NewTitle",
        "attr=v",
        "text=replaced",
        "class=cls",
        "removed=true",
        "after=replacedkid",
    ]);
}
