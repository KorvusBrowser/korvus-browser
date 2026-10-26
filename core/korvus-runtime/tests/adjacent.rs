//! before/after/replaceWith + insertAdjacentElement.
//! NB: `children` is a live HTMLCollection (core/dom/collections.rs), which — like the real
//! thing — has no `.map`; spread it through `Array.from` exactly as page script must.
use engine::Engine;
#[tokio::test]
async fn adjacent_methods() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><ul id="l"><li id="b">b</li></ul></body></html>"#).unwrap();
    e.eval("a", r#"
        const ul = document.getElementById("l");
        const b = document.getElementById("b");
        const a = document.createElement("li"); a.append("a");
        const c = document.createElement("li"); c.append("c");
        b.before(a);
        b.after(c);
        print("order=" + Array.from(ul.children).map(x => x.textContent).join(","));
        const z = document.createElement("li"); z.append("z");
        b.replaceWith(z);
        print("replaced=" + Array.from(ul.children).map(x => x.textContent).join(","));
        const d = document.createElement("li"); d.append("d");
        ul.insertAdjacentElement("afterbegin", d);
        print("adjacent=" + Array.from(ul.children).map(x => x.textContent).join(","));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "order=a,b,c",
        "replaced=a,z,c",
        "adjacent=d,a,z,c",
    ]);
}
