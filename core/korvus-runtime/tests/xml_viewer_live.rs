//! A top-level XML document with no style information is Chrome's XML TREE VIEWER, and
//! `document.contentType` is the response's type.
//!
//! Measured on Chrome 150 (fixture `X-xml-viewer`): an S3 `NoSuchKey` error body served as
//! `application/xml` becomes an XHTML document -- root `html`, a body -- whose text reads the source's
//! text, then the viewer's header, then the tree. korvus kept the raw XML: no body, no text, and a
//! `contentType` of `text/html`. The expected strings are Chrome's.
use engine::Engine;

const S3_ERROR: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message><Key>index.html</Key></Error>";

/// The fixture's reading: the body's text nodes (not script/style), joined by spaces, collapsed.
const READ: &str = r#"JSON.stringify((function(){var parts=[];var walk=function(n){for(var c=n.firstChild;c;c=c.nextSibling){if(c.nodeType===3)parts.push(c.nodeValue);else if(c.nodeType===1&&!{SCRIPT:1,STYLE:1,NOSCRIPT:1,TEMPLATE:1}[c.tagName])walk(c)}};if(document.body)walk(document.body);return {root:document.documentElement&&document.documentElement.nodeName, body:!!document.body, title:document.title, ct:document.contentType, text:parts.join(" ").replace(/\s+/g," ").trim()}})())"#;

async fn load(body: &str, content_type: &str) -> String {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page_typed(body, "http://127.0.0.1/e.xml", &[], Some(content_type)).await.expect("load");
    e.run().await.expect("run");
    let v = e.eval_value("read", READ.to_string()).expect("eval");
    v.as_str().unwrap().to_string()
}

#[tokio::test]
async fn an_unstyled_xml_page_is_the_tree_viewer_chrome_shows() {
    let got = load(S3_ERROR, "application/xml").await;
    assert_eq!(
        got,
        r#"{"root":"html","body":true,"title":"","ct":"application/xml","text":"NoSuchKey The specified key does not exist. index.html This XML file does not appear to have any style information associated with it. The document tree is shown below. <Error > <Code > NoSuchKey </Code > <Message > The specified key does not exist. </Message > <Key > index.html </Key > ... </Error >"}"#
    );
}

#[tokio::test]
async fn content_type_is_the_responses_and_an_xhtml_page_is_not_rewritten() {
    // `text/xml` is its own answer, not `application/xml`.
    let got = load(S3_ERROR, "text/xml; charset=utf-8").await;
    assert!(got.contains(r#""ct":"text/xml""#), "{got}");
    // An XHTML document has elements in a known namespace: no viewer, the page is itself.
    let xhtml = "<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>t</title></head><body><p>hi</p></body></html>";
    let got = load(xhtml, "application/xhtml+xml").await;
    assert_eq!(got, r#"{"root":"html","body":true,"title":"t","ct":"application/xhtml+xml","text":"hi"}"#);
}
