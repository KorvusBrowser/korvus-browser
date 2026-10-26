//! :first-of-type / :last-of-type / :nth-of-type / :only-of-type selectors.
use engine::Engine;
#[tokio::test]
async fn of_type_pseudos() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><div id="c">
        <h2>A</h2><p>1</p><p>2</p><p>3</p><span>S</span>
    </div></body></html>"#).unwrap();
    e.eval("a", r#"
        const c = document.getElementById("c");
        print("first=" + c.querySelector("p:first-of-type").textContent);
        print("last=" + c.querySelector("p:last-of-type").textContent);
        print("nth2=" + c.querySelector("p:nth-of-type(2)").textContent);
        print("nthLast=" + c.querySelector("p:nth-last-of-type(1)").textContent);
        print("only=" + c.querySelectorAll("span:only-of-type").length);
        print("h2only=" + c.querySelectorAll("h2:only-of-type").length);
        print("allP=" + Array.from(c.querySelectorAll("p:nth-of-type(odd)")).map(x => x.textContent).join(","));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "first=1",
        "last=3",
        "nth2=2",
        "nthLast=3",
        "only=1",
        "h2only=1",
        "allP=1,3",
    ]);
}
