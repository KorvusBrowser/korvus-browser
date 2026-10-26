//! G1.6 follow-up: inline <script> tags execute on load (in document order).
use engine::Engine;
#[tokio::test]
async fn inline_scripts_run() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body>
        <script>globalThis.__a = 1;</script>
        <script>var t=0; for(var i=0;i<1000;i++){t+=i;} globalThis.__b = t;</script>
        <script>print("script ran a=" + globalThis.__a + " b=" + globalThis.__b);</script>
    </body></html>"#).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec!["script ran a=1 b=499500"]);
}
