//! G1.5 gate: parse real HTML into the arena, walk children, read tagName/attributes/text.
use webapi::dom::{Dom, NodeKind};

#[test]
fn parse_walk_read() {
    let html = r#"<!doctype html><html><head><title>Hi</title></head>
      <body><div id="main" class="box"><p>Hello <b>world</b></p></div></body></html>"#;
    let dom = Dom::parse(html);

    let title = dom.get_elements_by_tag_name("title");
    assert_eq!(title.len(), 1);
    assert_eq!(dom.text_content(title[0]), "Hi");

    let divs = dom.get_elements_by_tag_name("div");
    assert_eq!(divs.len(), 1);
    assert_eq!(dom.get_attribute(divs[0], "id"), Some("main"));
    assert_eq!(dom.get_attribute(divs[0], "class"), Some("box"));
    assert_eq!(dom.get_attribute(divs[0], "missing"), None);

    let p = dom.get_elements_by_tag_name("p");
    assert_eq!(dom.text_content(p[0]), "Hello world");

    let root = dom.root();
    assert_eq!(dom.node(root).kind, NodeKind::Document);
    assert_eq!(dom.get_elements_by_tag_name("html").len(), 1);
    assert_eq!(dom.get_elements_by_tag_name("body").len(), 1);
}
