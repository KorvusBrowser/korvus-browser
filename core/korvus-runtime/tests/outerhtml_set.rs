//! outerHTML setter replaces element with parsed markup.
use engine::Engine;
#[tokio::test]
async fn outer_html_setter() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><ul id="l"><li id="a">a</li><li id="b">b</li><li id="c">c</li></ul></body></html>"#).unwrap();
    e.eval("a", r#"
        const b = document.getElementById("b");
        b.outerHTML = "<li id='x'>x</li><li id='y'>y</li>";
        const ul = document.getElementById("l");
        print("order=" + Array.from(ul.children).map(n => n.id).join(","));
        print("text=" + Array.from(ul.children).map(n => n.textContent).join(","));
        print("gone=" + (document.getElementById("b") === null));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "order=a,x,y,c",
        "text=a,x,y,c",
        "gone=true",
    ]);
}
