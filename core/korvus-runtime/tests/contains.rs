//! element.contains + document.activeElement.
use engine::Engine;
#[tokio::test]
async fn contains_and_active() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><div id="a"><span id="b">x</span></div><p id="c">y</p></body></html>"#).unwrap();
    e.eval("c", r#"
        const a = document.getElementById("a");
        const b = document.getElementById("b");
        const c = document.getElementById("c");
        print("aContainsB=" + a.contains(b));
        print("aContainsC=" + a.contains(c));
        print("aContainsA=" + a.contains(a));
        print("active=" + document.activeElement.tagName);
        b.scrollIntoView();  // no-op, must not throw
        print("scrolled=ok");
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "aContainsB=true",
        "aContainsC=false",
        "aContainsA=true",
        "active=BODY",
        "scrolled=ok",
    ]);
}
