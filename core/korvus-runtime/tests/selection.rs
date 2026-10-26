//! Selection/Range + document.createRange/getSelection.
use engine::Engine;
#[tokio::test]
async fn selection_range() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("s", r#"
        const r = document.createRange();
        r.selectNodeContents(document.body);  // no-op, must not throw
        print("range=" + (r instanceof Range) + " rect=" + r.getBoundingClientRect().width);
        const sel = getSelection();
        print("sel=" + (sel instanceof Selection) + " count=" + sel.rangeCount + " str='" + sel.toString() + "'");
        const docSel = document.getSelection();
        print("docSel=" + (docSel instanceof Selection));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "range=true rect=0",
        "sel=true count=0 str=''",
        "docSel=true",
    ]);
}

/// A FRAME document has its own Selection: `contentDocument.getSelection()` is the frame's, the
/// same object as the frame window's, not the page's, and it takes ranges over the FRAME's nodes -- a shadow
/// root in the frame answers it too. It answered null (and, before the frame's `contentWindow.document` was
/// the frame's own document, frame code landed on the page's selection).
#[tokio::test]
async fn a_frame_document_has_its_own_selection() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_page("<!doctype html><html><body></body></html>", "http://127.0.0.1:8123/page.html", &[]).await.unwrap();
    let v = e
        .eval_value(
            "frame-sel",
            r#"(() => {
                const f = document.body.appendChild(document.createElement("iframe"));
                const d = f.contentWindow.document;
                const s = d.getSelection();
                const host = d.body.appendChild(d.createElement("div"));
                const root = host.attachShadow({ mode: "open" });
                const span = root.appendChild(d.createElement("span"));
                span.textContent = "Some text";
                const r = d.createRange();
                r.setStart(span.firstChild, 0);
                r.setEnd(span.firstChild, 3);
                root.getSelection().removeAllRanges();
                root.getSelection().addRange(r);
                // A shadow root's getSelection() is its OWN object over the document's selection
                // (stock Chrome 150: tools/realweb/seed/shadowsel) -- not the document's.
                return JSON.stringify([s !== null, s !== document.getSelection(), s === f.contentWindow.getSelection(),
                                       s !== root.getSelection(), s.toString(), document.getSelection().toString(),
                                       new Document().getSelection()]);
            })()"#
                .to_string(),
        )
        .unwrap();
    assert_eq!(v, serde_json::json!(r#"[true,true,true,true,"Som","",null]"#), "frame selection");
}
