//! MouseEvent/KeyboardEvent/InputEvent constructors + click() bubbling.
use engine::Engine;
#[tokio::test]
async fn typed_events() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><form id="f"><button id="b">go</button></form></body></html>"#).unwrap();
    e.eval("a", r#"
        const me = new MouseEvent("click", { clientX: 10, clientY: 20, button: 1, ctrlKey: true, bubbles: true });
        print("mouse=" + me.clientX + "," + me.clientY + "," + me.button + "," + me.ctrlKey + "," + (me instanceof Event));
        // UI Events names the key "Control"; "Ctrl" is not a modifier key name and Chrome answers
        // false for it. The assertion below expected true for "Ctrl", which was the stub agreeing.
        print("mod=" + me.getModifierState("Ctrl") + "," + me.getModifierState("Control"));
        const ke = new KeyboardEvent("keydown", { key: "Enter", code: "Enter", shiftKey: true });
        print("key=" + ke.key + "," + ke.code + "," + ke.shiftKey);
        const ie = new InputEvent("input", { data: "x", inputType: "insertText" });
        print("input=" + ie.data + "," + ie.inputType);
        // click() bubbles to the form.
        const seen = [];
        const f = document.getElementById("f");
        f.addEventListener("click", (ev) => seen.push("form:" + ev.type + ":" + (ev.target.id) + ":" + (ev instanceof MouseEvent)));
        document.getElementById("b").click();
        print("click=" + seen.join(","));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "mouse=10,20,1,true,true",
        "mod=false,true",
        "key=Enter,Enter,true",
        "input=x,insertText",
        "click=form:click:b:true",
    ]);
}
