//! `xml_viewer.rs` -- what a top-level XML document with no style information BECOMES.
//!
//! Chrome does not leave such a document as it was parsed. When XML parsing ends, before
//! `DOMContentLoaded`, Blink's XML tree viewer rewrites the document into an XHTML one: a `<head>`
//! holding the viewer's style sheet, and a `<body>` holding, in order, a hidden copy of the source
//! nodes, the header "This XML file does not appear to have any style information associated with it.
//! The document tree is shown below.", and a pretty-printed tree of the source. A page, a probe and a
//! text extractor all read THAT document. Measured on Chrome 150 (fixture `X-xml-viewer`): the root is
//! `html`, `document.body` exists, and the body's text nodes read the source's text, then the header,
//! then the tree token by token (`<Error` and `>` are separate nodes, a folded element ends in `...`).
//! korvus kept the raw XML, so the same S3 `NoSuchKey` page had no body and no text at all.
//!
//! This is our own construction of that output, built from the measured text and the rule Blink
//! applies. Chromium's script and style sheet are not copied (embedding them is a licence question).
//!
//! WHEN it applies (Blink's `XMLDocumentParser::end`): the document parsed without error, has no
//! element in the XHTML, SVG or MathML namespace, carries no `xml-stylesheet` instruction that names a
//! CSS or XSLT sheet, and is the TOP-LEVEL document. The caller owns the last condition: only the page
//! load calls [`transform`], so a frame's XML document stays as parsed, as it does in Chrome. A parse
//! error needs no test of its own -- the parser reports it as an XHTML `<parsererror>`, which is an
//! element in a known namespace.

use std::collections::VecDeque;

use webapi::dom::{Dom, ElementNs, NodeId, NodeKind};

/// The header's text. Chrome localises it; the persona's browser runs in English.
const HEADER: &str =
    "This XML file does not appear to have any style information associated with it. The document tree is shown below.";

/// The viewer's style sheet: the source copy is hidden and the tree is indented monospace. Ours, not
/// Chromium's; what matters to a text reader is that the copy and the fold hints do not render.
const STYLE: &str = "#webkit-xml-viewer-source-xml { display: none; }\n\
.header { border-bottom: 2px solid black; padding-bottom: 5px; margin: 10px; }\n\
.pretty-print { margin-top: 1em; margin-left: 20px; font-family: monospace; font-size: 13px; }\n\
.opened { margin-left: 1em; }\n\
.folder > .hidden { display: none; }\n\
.folder-button { display: inline-block; width: 10px; margin-left: -10px; cursor: pointer; user-select: none; }\n\
.html-tag { color: #881280; }\n\
.html-attribute-name { color: #994500; }\n\
.html-attribute-value { color: #1a1aa6; }\n\
.comment { color: #236e25; white-space: pre; }\n";

/// An element whose only child is a text node this short (after trimming) is drawn on one line.
const SHORT_TEXT: usize = 50;

/// Does Chrome show this parsed XML document through the tree viewer? (The top-level condition is
/// the caller's.)
pub(crate) fn applies(dom: &Dom) -> bool {
    let doc = dom.root();
    if dom.first_element_child(doc).is_none() {
        return false;
    }
    let mut plain = true;
    dom.for_each_descendant(doc, |n| {
        if let NodeKind::Element { ns: ElementNs::Html | ElementNs::Svg | ElementNs::MathMl, .. } = &dom.node(n).kind {
            plain = false;
        }
    });
    plain && !dom.children(doc).into_iter().any(|n| names_a_style_sheet(dom, n))
}

/// A document-level `<?xml-stylesheet?>` whose `type` is CSS (or absent) or one of the XSLT types.
/// One with any other type styles nothing, so the viewer still runs.
fn names_a_style_sheet(dom: &Dom, n: NodeId) -> bool {
    let NodeKind::ProcessingInstruction(pi) = &dom.node(n).kind else { return false };
    let (target, data) = (&pi.target, &pi.data);
    if target != "xml-stylesheet" {
        return false;
    }
    let ty = pseudo_attribute(data, "type").unwrap_or_default();
    matches!(
        ty.as_str(),
        "" | "text/css" | "text/xml" | "text/xsl" | "application/xml" | "application/xhtml+xml" | "application/rss+xml" | "application/atom+xml"
    )
}

/// One `name="value"` (or single-quoted) pseudo-attribute of a processing instruction's data.
fn pseudo_attribute(data: &str, name: &str) -> Option<String> {
    let mut rest = data;
    while let Some(at) = rest.find(name) {
        let after = rest[at + name.len()..].trim_start();
        let boundary = at == 0 || rest[..at].ends_with(|c: char| c.is_ascii_whitespace());
        if boundary {
            if let Some(v) = after.strip_prefix('=') {
                let v = v.trim_start();
                let quote = v.chars().next()?;
                if quote == '"' || quote == '\'' {
                    let body = &v[1..];
                    return body.find(quote).map(|end| body[..end].to_string());
                }
            }
        }
        rest = &rest[at + name.len()..];
    }
    None
}

/// Rewrite the parsed document into the viewer's XHTML document.
pub(crate) fn transform(dom: &mut Dom) {
    let doc = dom.root();
    let html = element(dom, "html", None);
    let head = element(dom, "head", None);
    dom.append_child(html, head);
    let style = element(dom, "style", None);
    dom.set_attribute(style, "id", "xml-viewer-style");
    dom.append_child(head, style);
    let body = element(dom, "body", None);
    dom.append_child(html, body);
    let source = element(dom, "div", None);
    dom.set_attribute(source, "id", "webkit-xml-viewer-source-xml");
    dom.append_child(body, source);

    // Every node of the document moves into the hidden copy, except the doctype, which is dropped.
    for child in dom.children(doc) {
        dom.remove_child(doc, child);
        if dom.node_type(child) != 10 {
            dom.append_child(source, child);
        }
    }
    dom.append_child(doc, html);

    let header = element(dom, "div", Some("header"));
    dom.append_child(body, header);
    let message = element(dom, "span", None);
    append_text(dom, message, HEADER);
    dom.append_child(header, message);
    let br = element(dom, "br", None);
    dom.append_child(header, br);

    let tree = element(dom, "div", Some("pretty-print"));
    dom.append_child(body, tree);
    // Breadth-first over a queue, as Blink does it, so a deeply nested document cannot exhaust the
    // stack. Each node lands in the container named with it, so the tree is the same as a recursion's.
    let mut work: VecDeque<(NodeId, NodeId)> = dom.children(source).into_iter().map(|c| (tree, c)).collect();
    while let Some((parent, node)) = work.pop_front() {
        draw(dom, parent, node, &mut work);
    }
    append_text(dom, style, STYLE);
}

/// Draw one source node into `parent`; a folder queues its children for its content box.
fn draw(dom: &mut Dom, parent: NodeId, node: NodeId, work: &mut VecDeque<(NodeId, NodeId)>) {
    match dom.node(node).kind.clone() {
        NodeKind::Element { .. } => draw_element(dom, parent, node, work),
        NodeKind::Text(t) => {
            if !blank(&t) {
                let span = text_span(dom, &t, None);
                one_line(dom, parent, &[span]);
            }
        }
        NodeKind::Comment(t) => {
            let span = text_span(dom, &format!("<!--{t}-->"), Some("comment"));
            one_line(dom, parent, &[span]);
        }
        NodeKind::ProcessingInstruction(pi) => {
            let span = text_span(dom, &format!("<?{} {}?>", pi.target, pi.data), Some("html-tag"));
            one_line(dom, parent, &[span]);
        }
        NodeKind::CData(t) => {
            let span = text_span(dom, &format!("<![CDATA[{t}]]>"), None);
            one_line(dom, parent, &[span]);
        }
        _ => {}
    }
}

fn draw_element(dom: &mut Dom, parent: NodeId, node: NodeId, work: &mut VecDeque<(NodeId, NodeId)>) {
    let Some(first) = dom.first_child(node) else {
        let tag = tag(dom, node, Tag::Empty);
        one_line(dom, parent, &[tag]);
        return;
    };
    if let NodeKind::Text(t) = dom.node(first).kind.clone() {
        if dom.next_sibling(first).is_none() && short(&t) {
            let open = tag(dom, node, Tag::Open);
            let text = text_span(dom, &t, None);
            let close = tag(dom, node, Tag::Close);
            one_line(dom, parent, &[open, text, close]);
            return;
        }
    }
    // A folder: its opening line (the fold button, then the tag), the children's box, the `...` a
    // folded element shows in their place, and the closing line.
    let folder = element(dom, "div", Some("folder"));
    let start = element(dom, "div", Some("line"));
    let button = element(dom, "span", Some("folder-button fold"));
    dom.append_child(start, button);
    let open = tag(dom, node, Tag::Open);
    dom.append_child(start, open);
    dom.append_child(folder, start);
    let content = element(dom, "div", Some("opened"));
    dom.append_child(folder, content);
    let hint = text_span(dom, "...", Some("hidden"));
    dom.append_child(folder, hint);
    let close = tag(dom, node, Tag::Close);
    let end = element(dom, "div", Some("line"));
    dom.append_child(end, close);
    dom.append_child(folder, end);
    dom.append_child(parent, folder);
    for child in dom.children(node) {
        work.push_back((content, child));
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Tag {
    Open,
    Close,
    Empty,
}

/// `<name attr="value">`, `</name>` or `<name/>`, as separate text nodes around the attributes: the
/// measured text reads `<Error >`, so `<Error` and `>` are two nodes.
fn tag(dom: &mut Dom, node: NodeId, kind: Tag) -> NodeId {
    let span = element(dom, "span", Some("html-tag"));
    let name = dom.node_name(node);
    let before = if kind == Tag::Close { format!("</{name}") } else { format!("<{name}") };
    append_text(dom, span, &before);
    if kind != Tag::Close {
        let attrs = match &dom.node(node).kind {
            NodeKind::Element { attrs, .. } => attrs.clone(),
            _ => Vec::new(),
        };
        for (k, v) in attrs {
            let attr = element(dom, "span", Some("html-attribute"));
            append_text(dom, attr, " ");
            let n = text_span(dom, &k, Some("html-attribute-name"));
            dom.append_child(attr, n);
            append_text(dom, attr, "=\"");
            let value = text_span(dom, &v, Some("html-attribute-value"));
            dom.append_child(attr, value);
            append_text(dom, attr, "\"");
            dom.append_child(span, attr);
        }
    }
    append_text(dom, span, if kind == Tag::Empty { "/>" } else { ">" });
    span
}

fn one_line(dom: &mut Dom, parent: NodeId, parts: &[NodeId]) {
    let line = element(dom, "div", Some("line"));
    for &p in parts {
        dom.append_child(line, p);
    }
    dom.append_child(parent, line);
}

/// An XHTML element, with `class` when given.
fn element(dom: &mut Dom, name: &str, class: Option<&str>) -> NodeId {
    let el = dom.create_element_ns(name, ElementNs::Html);
    if let Some(c) = class {
        dom.set_attribute(el, "class", c);
    }
    el
}

fn text_span(dom: &mut Dom, text: &str, class: Option<&str>) -> NodeId {
    let span = element(dom, "span", class);
    append_text(dom, span, text);
    span
}

fn append_text(dom: &mut Dom, parent: NodeId, text: &str) {
    let t = dom.create_text_node(text);
    dom.append_child(parent, t);
}

/// JavaScript's `\s`, which is what the viewer's trim uses: it is not Rust's `char::is_whitespace`
/// (JS counts U+FEFF and not U+0085).
fn js_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}'
        | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

fn blank(s: &str) -> bool {
    s.chars().all(js_space)
}

/// Short means at most [`SHORT_TEXT`] UTF-16 code units once trimmed -- a JS string length.
fn short(s: &str) -> bool {
    s.trim_matches(js_space).encode_utf16().count() <= SHORT_TEXT
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The body's text nodes joined by spaces, whitespace collapsed -- the fixture's own reading.
    fn body_text(dom: &Dom) -> String {
        let html = dom.first_element_child(dom.root()).unwrap();
        let body = dom.child_elements(html)[1];
        let mut parts = Vec::new();
        dom.for_each_descendant(body, |n| {
            if let NodeKind::Text(t) = &dom.node(n).kind {
                parts.push(t.clone());
            }
        });
        parts.join(" ").split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn an_s3_error_document_reads_as_chrome_measured_it() {
        let mut dom = Dom::parse_xml(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message><Key>index.html</Key></Error>",
        );
        assert!(applies(&dom));
        transform(&mut dom);
        assert_eq!(dom.node_name(dom.first_element_child(dom.root()).unwrap()), "html");
        assert_eq!(
            body_text(&dom),
            "NoSuchKey The specified key does not exist. index.html This XML file does not appear to have any style \
             information associated with it. The document tree is shown below. <Error > <Code > NoSuchKey </Code > \
             <Message > The specified key does not exist. </Message > <Key > index.html </Key > ... </Error >"
        );
    }

    #[test]
    fn a_styled_or_namespaced_document_is_left_alone() {
        assert!(!applies(&Dom::parse_xml("<?xml-stylesheet href=\"a.css\"?><r/>")));
        assert!(!applies(&Dom::parse_xml("<?xml-stylesheet type=\"text/xsl\" href=\"a.xsl\"?><r/>")));
        assert!(!applies(&Dom::parse_xml("<html xmlns=\"http://www.w3.org/1999/xhtml\"><body/></html>")));
        assert!(!applies(&Dom::parse_xml("<r><svg xmlns=\"http://www.w3.org/2000/svg\"/></r>")));
        assert!(!applies(&Dom::parse_xml("<r></x>")), "a parse error shows the error page, not the tree");
        // A sheet of a type nothing applies does not count.
        assert!(applies(&Dom::parse_xml("<?xml-stylesheet type=\"text/plain\" href=\"a\"?><r/>")));
    }

    #[test]
    fn attributes_empty_elements_and_long_text_are_drawn_as_the_viewer_draws_them() {
        let long = "x".repeat(51);
        let mut dom = Dom::parse_xml(&format!("<r a=\"1\"><e/><t>{long}</t></r>"));
        transform(&mut dom);
        let text = body_text(&dom);
        let tree = text.split("shown below. ").nth(1).unwrap();
        assert_eq!(tree, format!("<r a =\" 1 \" > <e /> <t > {long} ... </t > ... </r >"));
    }
}
