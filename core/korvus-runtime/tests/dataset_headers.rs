//! element.dataset + Headers.
use engine::Engine;
#[tokio::test]
async fn dataset_and_headers() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><div id="d" data-user-id="42" data-role="admin"></div></body></html>"#).unwrap();
    e.eval("dh", r#"
        const d = document.getElementById("d");
        print("userId=" + d.dataset.userId + " role=" + d.dataset.role);
        d.dataset.newKey = "v1";
        print("set=" + d.getAttribute("data-new-key") + " has=" + ("userId" in d.dataset));
        const h = new Headers({ "Content-Type": "text/html" });
        h.append("X-Test", "1"); h.append("X-Test", "2");
        print("h=" + h.get("content-type") + " x=" + h.get("x-test") + " has=" + h.has("X-Test"));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "userId=42 role=admin",
        "set=v1 has=true",
        "h=text/html x=1, 2 has=true",
    ]);
}
