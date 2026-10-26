//! document.cookie get/set (name=value pairs; attributes ignored).
use engine::Engine;
#[tokio::test]
async fn document_cookie() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("c", r#"
        print("empty=" + JSON.stringify(document.cookie));
        document.cookie = "a=1; Path=/; Secure";
        document.cookie = "b=2";
        print("a=" + (document.cookie.includes("a=1")));
        print("b=" + (document.cookie.includes("b=2")));
        document.cookie = "a=9";
        print("upd=" + (document.cookie.includes("a=9") && !document.cookie.includes("a=1")));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec!["empty=\"\"", "a=true", "b=true", "upd=true"]);
}
