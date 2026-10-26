//! Reflected form/element properties + form methods.
use engine::Engine;
#[tokio::test]
async fn reflected_props() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><form id="f" action="/go" method="post">
        <input id="i" type="checkbox" checked disabled><img id="m" src="/a.png" alt="pic"></form></body></html>"#).unwrap();
    e.eval("r", r#"
        const i = document.getElementById("i");
        print("checked=" + i.checked + " disabled=" + i.disabled + " type=" + i.type);
        // Setting `.checked` sets the dirty checkedness and does NOT touch the `checked` attribute,
        // which `defaultChecked` reflects. Chrome 151 keeps the attribute; this asserted it vanished.
        i.checked = false;
        print("uncheck=" + i.checked + " hasAttr=" + i.hasAttribute("checked") + " default=" + i.defaultChecked);
        const m = document.getElementById("m");
        print("img=" + m.src + "," + m.alt);
        const f = document.getElementById("f");
        print("form=" + f.action + "," + f.method + " elems=" + f.elements.length);
        f.submit();  // no-op, must not throw
        print("submit=ok");
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "checked=true disabled=true type=checkbox",
        "uncheck=false hasAttr=true default=true",
        "img=/a.png,pic",
        "form=/go,post elems=1",
        "submit=ok",
    ]);
}
