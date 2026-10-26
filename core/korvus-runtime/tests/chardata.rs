//! CharacterData mutation methods on text nodes.
use engine::Engine;
#[tokio::test]
async fn character_data() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><p id="p">abcdef</p></body></html>"#).unwrap();
    e.eval("a", r#"
        const t = document.getElementById("p").firstChild;
        print("sub=" + t.substringData(1, 3));
        t.appendData("ghi"); print("append=" + t.data);
        t.insertData(3, "XXX"); print("insert=" + t.data);
        t.deleteData(3, 3); print("delete=" + t.data);
        t.replaceData(0, 3, "YYY"); print("replace=" + t.data);
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "sub=bcd",
        "append=abcdefghi",
        "insert=abcXXXdefghi",
        "delete=abcdefghi",
        "replace=YYYdefghi",
    ]);
}
