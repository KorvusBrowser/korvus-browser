//! querySelector/querySelectorAll (CSS #id/.class/tag) + getElementsByClassName.
use engine::Engine;
#[tokio::test]
async fn query_selectors() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(
        r##"<!doctype html><html><body>
        <div id="main" class="box big">A</div>
        <p class="box">B</p><p class="box">C</p><span>D</span>
    </body></html>"##,
    )
    .unwrap();
    e.eval(
        "q",
        r##"
        print("byId=" + document.querySelector("#main").textContent);
        print("byTag=" + document.querySelector("span").textContent);
        print("byClass=" + document.querySelector(".box").textContent);
        print("allBox=" + Array.from(document.querySelectorAll(".box")).map(el => el.textContent).join(","));
        print("allP=" + document.querySelectorAll("p").length);
        print("gebc=" + document.getElementsByClassName("box").length);
        print("miss=" + (document.querySelector("#nope") === null));
    "##
        .into(),
    )
    .unwrap();
    e.run().await.unwrap();
    assert_eq!(
        e.take_output(),
        vec!["byId=A", "byTag=D", "byClass=A", "allBox=A,B,C", "allP=2", "gebc=3", "miss=true"]
    );
}
