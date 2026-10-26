//! document.forms/images/scripts/links + getElementsByName + scoped getElementsByTagName.
use engine::Engine;
#[tokio::test]
async fn doc_collections() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body>
        <form></form><form></form>
        <img src="a"><a href="/1">x</a><a>noHref</a>
        <input name="u"><input name="u"><input name="p">
        <div id="d"><span>1</span><span>2</span></div></body></html>"#).unwrap();
    e.eval("c", r#"
        print("forms=" + document.forms.length + " images=" + document.images.length + " links=" + document.links.length);
        print("byName=" + document.getElementsByName("u").length);
        print("all=" + (document.querySelectorAll("*").length > 5));
        const d = document.getElementById("d");
        print("scoped=" + d.getElementsByTagName("span").length);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "forms=2 images=1 links=1",
        "byName=2",
        "all=true",
        "scoped=2",
    ]);
}
