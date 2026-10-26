//! A second document installed into the SAME runtime must not inherit the first one's id-keyed
//! tables. `Dom::parse` numbers a new document's nodes from zero, so a wrapper, an
//! owner document or a qualified name recorded for id N answered for whatever node is id N in the new
//! document: the quality harness reloads its page between interfaces, and after one reload
//! `createElement("input")` came back wearing `HTMLOptionElement.prototype` (its id had been an
//! `<option>` made with `createElementNS`) -- 589 members graded wrong in `tools/status/data/status.json`.
use engine::Engine;

const PAGE: &str = r#"<!doctype html><html><head></head><body><p id="p">x</p></body></html>"#;

#[tokio::test]
async fn a_reinstalled_document_forgets_the_old_documents_node_tables() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();
    // Fill the ids after the parse with elements whose wrappers and qualified names get recorded.
    e.eval("first", r#"
        globalThis.keep = [];
        for (let i = 0; i < 100; i++) {
            keep.push(document.createElementNS("http://www.w3.org/1999/xhtml", "option"));
            keep.push(document.createElement("select"));
        }
    "#.into()).unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();
    e.eval("second", r#"
        const seen = new Set();
        for (let i = 0; i < 200; i++) {
            const el = document.createElement("input");
            seen.add(Object.prototype.toString.call(el) + ":" + el.tagName + ":" + typeof el.webkitdirectory);
        }
        print([...seen].join(" "));
        print(Object.prototype.toString.call(document.getElementById("p")));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec!["[object HTMLInputElement]:INPUT:boolean", "[object HTMLParagraphElement]"]);
}
