//! DOM events fire on elements/document (keyed by nodeId, persist across wrapper instances).
use engine::Engine;
#[tokio::test]
async fn dom_events() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><button id="b">x</button></body></html>"#).unwrap();
    e.eval("ev", r#"
        let hits = 0;
        document.getElementById("b").addEventListener("click", () => { hits++; });
        // dispatch on a DIFFERENT wrapper instance for the same node -> listener still fires
        const ok = document.getElementById("b").dispatchEvent(new Event("click"));
        document.getElementById("b").dispatchEvent(new Event("click"));
        print("hits=" + hits + " ok=" + ok);
        let docHit = 0;
        document.addEventListener("custom", () => docHit++);
        document.dispatchEvent(new Event("custom"));
        print("docHit=" + docHit);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec!["hits=2 ok=true", "docHit=1"]);
}
