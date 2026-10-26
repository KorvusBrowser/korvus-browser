//! LIVE gate: `XMLSerializer` and an XML document's `innerHTML` / `outerHTML` serialize as Chrome 150 does --
//! DOM Parsing's XML serialization with Blink's attribute prefixes and escapes (`core/xml/xml_serializer.rs`),
//! and the namespace, prefix and local name of a parsed XML element and of an attribute as the parsers decide
//! them.
//!
//! It answered the HTML serialization, read through the page-visible `outerHTML` / `nodeName` / `nodeType`
//! getters: every row of `fixtures/xml-serializer/probe.js` but the not-a-Node TypeError differed (no `xmlns`
//! on the root element, `<br>`, `<rect></rect>`, text unescaped, a comment as its bare data, a doctype and a
//! CDATA section as nothing), `<a:c>` of an XML document read prefix null and local name `a:c`, and every
//! attribute's namespace and prefix read null.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-09**: the script, served from
//! 127.0.0.1, answered `fixtures/xml-serializer/chrome-152.json` row for row. Not in it: an attribute
//! `setAttributeNS` gave a namespace no declaration names -- the arena keeps an attribute's qualified name and
//! no namespace, so `xmlns:z="urn:z"` cannot be written for it (Chrome writes it).

#[tokio::test]
async fn xml_serialization_answers_as_chrome_150() {
    let script = include_str!("fixtures/xml-serializer/probe.js");
    let want: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/xml-serializer/chrome-152.json")).expect("the recorded Chrome list");
    let got = crate::keyboard_live::answers(crate::keyboard_live::CHROME_BUNDLE, script).await;
    assert_eq!(got.len(), want.len(), "answer count: {got:?}");
    let differ: Vec<String> = got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("#{i}: korvus {g}\n    Chrome {w}")).collect();
    assert!(differ.is_empty(), "{} answers differ from Chrome 150:\n{}", differ.len(), differ.join("\n"));
}
