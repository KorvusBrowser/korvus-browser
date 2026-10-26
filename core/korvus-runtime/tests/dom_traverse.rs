//! element.matches/closest/querySelector(All)/hasAttribute/getAttributeNames.
use engine::Engine;
#[tokio::test]
async fn traverse() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r##"<!doctype html><html><body>
        <section id="s" class="wrap" data-k="v">
          <div class="row"><span class="c">a</span></div>
          <div class="row"><span class="c">b</span></div>
        </section></body></html>"##).unwrap();
    e.eval("t", r##"
        const s = document.getElementById("s");
        print("matches=" + s.matches(".wrap") + "," + s.matches("div"));
        print("hasAttr=" + s.hasAttribute("data-k") + "," + s.hasAttribute("nope"));
        print("names=" + s.getAttributeNames().sort().join(","));
        print("qs=" + s.querySelector(".c").textContent);
        print("qsa=" + Array.from(s.querySelectorAll(".c")).map(x => x.textContent).join(","));
        const span = s.querySelector(".c");
        print("closest=" + span.closest(".row").className + "," + span.closest("#s").id);
    "##.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "matches=true,false",
        "hasAttr=true,false",
        "names=class,data-k,id",
        "qs=a",
        "qsa=a,b",
        "closest=row,s",
    ]);
}
