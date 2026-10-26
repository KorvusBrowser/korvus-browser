//! `document.elementFromPoint`.
//!
//! It did not exist: with no layout there was nothing to test a point against, so a page asking
//! what is at a coordinate got `undefined` where every browser gives an element or `null`. A
//! detector that clicks something and then asks what it clicked sees that in one line.
use engine::Engine;
async fn run(html: &str, script: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(html).unwrap();
    e.eval("t", script.to_string()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

const PAGE: &str = r#"<!doctype html><html><body>
  <div id="outer" style="width:200px;height:100px">
    <div id="inner" style="width:50px;height:20px"></div>
  </div>
  <div id="after" style="width:100px;height:30px"></div>
</body></html>"#;

#[tokio::test]
async fn a_point_finds_the_innermost_element_under_it() {
    let out = run(
        PAGE,
        r#"
        const at = (x, y) => { const e = document.elementFromPoint(x, y); return e ? (e.id || e.tagName) : "null"; };
        print("inner=" + at(20, 15));
        print("outer=" + at(150, 80));
        print("after=" + at(20, 120));
        print("outside=" + at(-5, -5));
        print("beyond=" + at(5000, 5000));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec!["inner=inner", "outer=outer", "after=after", "outside=null", "beyond=null"]
    );
}

#[tokio::test]
async fn the_whole_stack_comes_back_topmost_first() {
    let out = run(
        PAGE,
        r#"
        const stack = document.elementsFromPoint(20, 15).map((e) => e.id || e.tagName);
        print(stack.join(","));
        "#,
    )
    .await;
    assert_eq!(out, vec!["inner,outer,BODY,HTML"]);
}

// The elements are the canonical wrappers, so identity holds against a query for the same node --
// a detector comparing `elementFromPoint(...) === document.getElementById(...)` expects `true`.
#[tokio::test]
async fn the_element_returned_is_the_same_object_a_query_returns() {
    let out = run(
        PAGE,
        r#"
        const hit = document.elementFromPoint(20, 15);
        print("same=" + (hit === document.getElementById("inner")));
        print("isElement=" + (hit instanceof Element));
        print("native=" + (document.elementFromPoint.toString().indexOf("[native code]") >= 0));
        "#,
    )
    .await;
    assert_eq!(out, vec!["same=true", "isElement=true", "native=true"]);
}

// `visibility: hidden` keeps its space but cannot be pointed at; `display: none` has no box at all.
#[tokio::test]
async fn invisible_and_unpointable_elements_are_not_hit() {
    let out = run(
        r#"<!doctype html><html><body>
             <div id="hidden" style="visibility:hidden;width:200px;height:50px"></div>
             <div id="unpointable" style="pointer-events:none;width:200px;height:50px"></div>
           </body></html>"#,
        r#"
        const at = (x, y) => { const e = document.elementFromPoint(x, y); return e ? (e.id || e.tagName) : "null"; };
        print("overHidden=" + at(50, 20));
        print("overUnpointable=" + at(50, 70));
        "#,
    )
    .await;
    // Both fall through to the element behind them, which is `<body>`.
    assert_eq!(out, vec!["overHidden=BODY", "overUnpointable=BODY"]);
}
