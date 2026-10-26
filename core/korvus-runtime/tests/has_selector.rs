//! :has() relational pseudo-class.
use engine::Engine;
#[tokio::test]
async fn has_pseudo() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body>
        <div class="card" id="c1"><img src="a.jpg"><p>has image</p></div>
        <div class="card" id="c2"><p>no image</p></div>
        <div class="card" id="c3"><span><img src="b.jpg"></span></div>
        <h2 id="h">Title</h2><p id="after">follows h2</p>
    </body></html>"#).unwrap();
    e.eval("a", r#"
        // Cards that directly contain an <img>.
        print("directImg=" + Array.from(document.querySelectorAll("div.card:has(> img)")).map(d => d.id).join(","));
        // Cards that contain an <img> anywhere.
        print("anyImg=" + Array.from(document.querySelectorAll("div.card:has(img)")).map(d => d.id).join(","));
        // h2 immediately followed by a <p>.
        print("nextP=" + Array.from(document.querySelectorAll("h2:has(+ p)")).map(d => d.id).join(","));
        // Cards with a <p> but no <img> (composed).
        print("pNoImg=" + Array.from(document.querySelectorAll("div.card:has(p)")).map(d => d.id).join(","));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "directImg=c1",
        "anyImg=c1,c3",
        "nextP=h",
        "pNoImg=c1,c2",
    ]);
}
