//! document.implementation.createHTMLDocument (sanitizer-style clean-tree pattern).
use engine::Engine;
#[tokio::test]
async fn create_html_document() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><p id="host">real page</p></body></html>"#).unwrap();
    e.eval("a", r#"
        const doc = document.implementation.createHTMLDocument("Clean");
        print("title=" + doc.title);
        print("emptyBody=" + doc.body.childNodes.length);
        // Sanitizer pattern: set body HTML on the clean doc, query it.
        doc.body.innerHTML = "<div class='c'>x</div><script>bad()<\/script><span>y</span>";
        print("divs=" + doc.querySelectorAll("div.c").length);
        print("spanText=" + doc.querySelector("span").textContent);
        // Isolation: clean doc must not see the host page.
        print("isolated=" + (doc.getElementById("host") === null));
        print("hostAlive=" + document.getElementById("host").textContent);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "title=Clean",
        "emptyBody=0",
        "divs=1",
        "spanText=y",
        "isolated=true",
        "hostAlive=real page",
    ]);
}
