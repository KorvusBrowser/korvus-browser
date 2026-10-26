//! G1.6: native DOM exposed to JS. document/Element getters are native and read the arena live.
use engine::Engine;
#[tokio::test]
async fn native_dom() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("init");
    e.load_html(r#"<!doctype html><html><head><title> Hello </title></head>
        <body><div id="main" class="box">Hi <b>there</b></div></body></html>"#).expect("load");
    e.eval("dom", r#"
        print("title=" + document.title);
        const el = document.getElementById("main");
        print("tag=" + el.tagName);
        print("class=" + el.className);
        print("text=" + el.textContent);
        print("attr=" + el.getAttribute("id"));
        print("missing=" + el.getAttribute("nope"));
        let gp = Object.getPrototypeOf(el), gd;
        while (gp && !(gd = Object.getOwnPropertyDescriptor(gp, "tagName"))) gp = Object.getPrototypeOf(gp);
        const g = gd.get;
        print("native=" + g.toString().includes("[native code]"));
        print("none=" + (document.getElementById("ghost") === null));
    "#.into()).expect("eval");
    e.run().await.expect("loop");
    assert_eq!(e.take_output(), vec![
        "title=Hello",
        "tag=DIV",
        "class=box",
        "text=Hi there",
        "attr=main",
        "missing=null",
        "native=true",
        "none=true",
    ]);
}
