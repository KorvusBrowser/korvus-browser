//! DOMParser.parseFromString + XMLSerializer over the shared arena.
use engine::Engine;
#[tokio::test]
async fn domparser_roundtrip() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><p id="main">host</p></body></html>"#).unwrap();
    e.eval("a", r#"
        const p = new DOMParser();
        const doc = p.parseFromString(
            "<html><head><title>Parsed</title></head><body><div class='x' id='a'>one</div><div class='x'>two</div></body></html>",
            "text/html");
        print("title=" + doc.title);
        print("body=" + (doc.body ? doc.body.tagName : "none"));
        print("byId=" + doc.getElementById("a").textContent);
        print("byClass=" + doc.getElementsByClassName("x").length);
        print("qsa=" + Array.from(doc.querySelectorAll("div.x")).map(d => d.textContent).join(","));
        // Isolation: the parsed doc must NOT see the host page's #main.
        print("isolated=" + (doc.getElementById("main") === null));
        print("host=" + document.getElementById("main").textContent);
        const xs = new XMLSerializer();
        print("ser=" + xs.serializeToString(doc.getElementById("a")));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "title=Parsed",
        "body=BODY",
        "byId=one",
        "byClass=2",
        "qsa=one,two",
        "isolated=true",
        "host=host",
        // XMLSerializer is XML serialization: an HTML element at the root names its namespace (stock Chrome 150)
        "ser=<div xmlns=\"http://www.w3.org/1999/xhtml\" class=\"x\" id=\"a\">one</div>",
    ]);
}
