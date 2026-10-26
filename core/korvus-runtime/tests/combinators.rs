//! CSS combinators: child (>), adjacent (+), general sibling (~).
use engine::Engine;
#[tokio::test]
async fn css_combinators() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r##"<!doctype html><html><body>
        <ul class="menu"><li>A</li><li class="sel">B</li><li>C</li><li>D</li></ul>
        <div><p>deep<span>x</span></p></div></body></html>"##).unwrap();
    e.eval("s", r##"
        // child combinator: direct li children of ul.menu
        print("child=" + document.querySelectorAll("ul.menu > li").length);
        // child must NOT match the span (not a direct child of p's parent div)
        print("childOnly=" + document.querySelectorAll("div > p").length + "," + document.querySelectorAll("div > span").length);
        // adjacent: li right after li.sel
        print("adjacent=" + document.querySelector("li.sel + li").textContent);
        // general sibling: all li after li.sel
        print("general=" + document.querySelectorAll("li.sel ~ li").length);
        // descendant still works
        print("desc=" + document.querySelector("div span").textContent);
    "##.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "child=4",
        "childOnly=1,0",
        "adjacent=C",
        "general=2",
        "desc=x",
    ]);
}
