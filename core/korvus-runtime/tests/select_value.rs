//! <select> options/selectedIndex/value + className/href/title/tabIndex reflection.
use engine::Engine;
#[tokio::test]
async fn select_and_reflection() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body>
        <a id="lnk" href="/x" class="nav big" title="Go">L</a>
        <select id="s">
            <option value="a">A</option>
            <option value="b" selected>B</option>
            <option value="c">C</option>
        </select>
    </body></html>"#).unwrap();
    e.eval("a", r#"
        const a = document.getElementById("lnk");
        print("cls=" + a.className + ",href=" + a.href + ",title=" + a.title);
        a.className = "one";
        print("cls2=" + a.getAttribute("class"));
        const s = document.getElementById("s");
        print("val=" + s.value + ",idx=" + s.selectedIndex + ",opts=" + s.options.length);
        s.value = "c";
        print("val2=" + s.value + ",idx2=" + s.selectedIndex);
        s.selectedIndex = 0;
        print("val3=" + s.value);
        print("tab=" + a.tabIndex); a.tabIndex = 3; print("tab2=" + a.tabIndex);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "cls=nav big,href=/x,title=Go",
        "cls2=one",
        "val=b,idx=1,opts=3",
        "val2=c,idx2=2",
        "val3=a",
        // An `<a href>` with no tabindex is 0 in Chrome 150 (tools/realweb/seed/ti), not -1.
        "tab=0",
        "tab2=3",
    ]);
}
