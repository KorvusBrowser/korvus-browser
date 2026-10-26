//! Inline `<svg>` in MARKUP is SVG — the namespace the HTML parser's foreign-content rules produce,
//! carried across the rcdom->arena walk onto the node itself (`korvus_dom::ElementNs`).
//!
//! Before this, `<svg><rect>` written in markup — how essentially every real page ships an icon, a
//! logo or a chart — became an `HTMLUnknownElement` in the XHTML namespace with `typeof rect.x ===
//! "undefined"`, while `createElementNS(svgNS,"rect")` gave a live `SVGRectElement`. Every expected
//! value below is Chrome 150 headless (`--dump-dom` over a loopback server) on the same markup.
use engine::Engine;

async fn probe(html: &str, script: &str) -> Vec<String> {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(html).unwrap();
    e.eval("a", script.into()).unwrap();
    e.run().await.unwrap();
    e.take_output()
}

/// The four rows the defect was measured on, against Chrome on the same page.
#[tokio::test]
async fn parser_created_svg_is_svg() {
    let out = probe(
        r#"<!doctype html><svg><rect x="10" y="2" width="5"/></svg>"#,
        r#"
        const r = document.getElementsByTagName("rect")[0];
        print("ctor=" + r.constructor.name);
        print("ns=" + r.namespaceURI);
        print("typeof_x=" + typeof r.x);
        print("ce_ns=" + document.createElementNS("http://www.w3.org/2000/svg", "rect").constructor.name);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "ctor=SVGRectElement",
            "ns=http://www.w3.org/2000/svg",
            "typeof_x=object",
            "ce_ns=SVGRectElement",
        ]
    );
}

/// SVG names are case-SENSITIVE, and html5ever's tag-name adjustment table is what produces the
/// camel case: the tokenizer sees `<clippath>`/`<febLend>` and the tree builder writes `clipPath` /
/// `feBlend`. An HTML element's `tagName` is uppercased; a foreign one's is not.
#[tokio::test]
async fn svg_tag_name_case_is_preserved() {
    let out = probe(
        r#"<!doctype html><div id="d"></div><svg><defs><clippath id="c"><rect x="1" y="2" width="30"/></clippath></defs><FEBLEND/></svg>"#,
        r#"
        const cp = document.getElementById("c");
        print("tagName=" + cp.tagName);
        print("nodeName=" + cp.nodeName);
        print("localName=" + cp.localName);
        print("ctor=" + cp.constructor.name);
        print("div=" + document.getElementById("d").tagName);
        // getElementsByTagName is case-SENSITIVE for a foreign element (and lower-cases only the
        // HTML-namespaced ones), where a type SELECTOR in an HTML document is case-insensitive for
        // both. Chrome 150, measured: gebtn("feBlend") 1 / gebtn("feblend") 0, while
        // querySelectorAll matches clipPath / clippath / CLIPPATH / ClipPath alike.
        print("feBlend=" + (document.getElementsByTagName("feBlend")[0] || {}).tagName);
        print("gebtn_lower=" + document.getElementsByTagName("feblend").length);
        print("qs=" + !!document.querySelector("clipPath"));
        print("qs_lower=" + !!document.querySelector("clippath"));
        print("qs_upper=" + !!document.querySelector("CLIPPATH"));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "tagName=clipPath",
            "nodeName=clipPath",
            "localName=clipPath",
            "ctor=SVGClipPathElement",
            "div=DIV",
            "feBlend=feBlend",
            "gebtn_lower=0",
            "qs=true",
            "qs_lower=true",
            "qs_upper=true",
        ]
    );
}

/// Selectors' "same type" is same LOCAL NAME **in the same namespace**, which only a tree that
/// records namespaces can answer. Chrome 150, measured on an HTML `<a>`, an SVG `<a>` and a second
/// HTML `<a>` as siblings: the SVG one is `:first-of-type` AND `:last-of-type` of its own type while
/// the second HTML `<a>` is `:nth-of-type(2)`, not `(3)`.
#[tokio::test]
async fn nth_of_type_counts_per_namespace() {
    let out = probe(
        r#"<!doctype html><div id="p"></div>"#,
        r#"
        const SVG = "http://www.w3.org/2000/svg";
        const d = document.getElementById("p");
        const h1 = document.createElement("a");
        const s1 = document.createElementNS(SVG, "a");
        const h2 = document.createElement("a");
        d.appendChild(h1); d.appendChild(s1); d.appendChild(h2);
        print("h1_nth1=" + h1.matches(":nth-of-type(1)"));
        print("s1_nth1=" + s1.matches(":nth-of-type(1)"));
        print("s1_nth2=" + s1.matches(":nth-of-type(2)"));
        print("h2_nth2=" + h2.matches(":nth-of-type(2)"));
        print("h2_nth3=" + h2.matches(":nth-of-type(3)"));
        print("s1_last=" + s1.matches(":last-of-type"));
        print("h2_last=" + h2.matches(":last-of-type"));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "h1_nth1=true",
            "s1_nth1=true",
            "s1_nth2=false",
            "h2_nth2=true",
            "h2_nth3=false",
            "s1_last=true",
            "h2_last=true",
        ]
    );
}

/// An animated member READ THROUGH to the element's own content attribute — the whole SVG value
/// surface that was unreachable from markup. `<rect width="30">` answers 30, and it stays live.
#[tokio::test]
async fn parsed_svg_animated_members_are_live() {
    let out = probe(
        r#"<!doctype html><svg><rect id="r" x="1" y="2" width="30"/></svg>"#,
        r#"
        const r = document.getElementById("r");
        print("x=" + r.x.baseVal.value);
        print("width=" + r.width.baseVal.value);
        print("animVal=" + r.width.animVal.value);
        print("same=" + (r.x === r.x));
        r.setAttribute("width", "44");
        print("live=" + r.width.baseVal.value);
        "#,
    )
    .await;
    assert_eq!(out, vec!["x=1", "width=30", "animVal=30", "same=true", "live=44"]);
}

/// The five local names that exist in BOTH namespaces and name a different interface in each. While
/// the parser dropped the namespace these all came back as the HTML interface (an SVG `<a>` was an
/// `HTMLAnchorElement`), which is exactly the kind of near-miss that passes a shallow probe.
#[tokio::test]
async fn dual_namespace_tag_names_pick_the_svg_interface() {
    let out = probe(
        r#"<!doctype html><a id="h" href="x">h</a><svg><a id="sa"><text id="st">t</text></a><title id="sti">ti</title><style id="sts"></style><script id="scr"></script></svg>"#,
        r#"
        for (const id of ["h", "sa", "st", "sti", "sts", "scr"]) {
          const e = document.getElementById(id);
          print(id + "=" + e.constructor.name);
        }
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "h=HTMLAnchorElement",
            "sa=SVGAElement",
            "st=SVGTextElement",
            "sti=SVGTitleElement",
            "sts=SVGStyleElement",
            "scr=SVGScriptElement",
        ]
    );
}

/// html5ever's foreign-attribute adjustment gives `xlink:href` a prefix and a namespace; keeping only
/// the LOCAL name collapsed it onto `href`, so an SVG `<a>` lost its link and `outerHTML` serialized
/// `<a href=…>`. The arena keys attributes by their qualified name, which is the convention
/// `setAttributeNS` already stored by.
#[tokio::test]
async fn foreign_attribute_keeps_its_prefix() {
    let out = probe(
        r#"<!doctype html><svg><a id="a" xlink:href="u" href="v">t</a></svg>"#,
        r#"
        const a = document.getElementById("a");
        print("qual=" + a.getAttribute("xlink:href"));
        print("ns=" + a.getAttributeNS("http://www.w3.org/1999/xlink", "href"));
        print("plain=" + a.getAttribute("href"));
        print("names=" + Array.from(a.attributes).map(x => x.name).join(","));
        print("outer=" + a.outerHTML);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "qual=u",
            "ns=u",
            "plain=v",
            "names=id,xlink:href,href",
            r#"outer=<a id="a" xlink:href="u" href="v">t</a>"#,
        ]
    );
}

/// Everything keyed on the namespace, in one page: `getElementsByTagName(NS)`, the HTML-only name
/// folding of `getElementsByTagName`, `cloneNode` (which carries the namespace because it is a field
/// on the node, where a side map keyed by node id could not), and `isEqualNode`.
#[tokio::test]
async fn namespace_keyed_queries_follow() {
    let out = probe(
        r#"<!doctype html><svg><rect id="r" x="1"/></svg><math><mi id="m">x</mi></math>"#,
        r#"
        const SVG = "http://www.w3.org/2000/svg";
        print("byNS=" + document.getElementsByTagNameNS(SVG, "*").length);
        print("byNSrect=" + document.getElementsByTagNameNS(SVG, "rect").length);
        print("math_ns=" + document.getElementById("m").namespaceURI);
        print("math_tag=" + document.getElementById("m").tagName);
        const r = document.getElementById("r");
        const c = r.cloneNode(true);
        print("clone_ns=" + c.namespaceURI);
        print("clone_ctor=" + c.constructor.name);
        print("equal=" + r.isEqualNode(c));
        const html_rect = document.createElement("rect");
        html_rect.setAttribute("id", "r"); html_rect.setAttribute("x", "1");
        print("cross_ns_equal=" + r.isEqualNode(html_rect));
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "byNS=2",
            "byNSrect=1",
            "math_ns=http://www.w3.org/1998/Math/MathML",
            "math_tag=mi",
            "clone_ns=http://www.w3.org/2000/svg",
            "clone_ctor=SVGRectElement",
            "equal=true",
            "cross_ns_equal=false",
        ]
    );
}

/// An HTML element that merely happens to be NAMED `svg` (one inside a `<template>`'s HTML content,
/// or created by `createElement`) is not in the SVG namespace, and the ancestor-walk guess the
/// fragment-context code used before this unit could not tell the two apart.
#[tokio::test]
async fn html_named_svg_is_not_svg() {
    let out = probe(
        r#"<!doctype html><div id="d"></div>"#,
        r#"
        const e = document.createElement("svg");
        print("ctor=" + e.constructor.name);
        print("ns=" + e.namespaceURI);
        const d = document.getElementById("d");
        d.innerHTML = "<svg><rect x='3'/></svg>";
        const r = d.getElementsByTagName("rect")[0];
        print("inner_ctor=" + r.constructor.name);
        print("inner_ns=" + r.namespaceURI);
        print("inner_x=" + r.x.baseVal.value);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "ctor=HTMLUnknownElement",
            "ns=http://www.w3.org/1999/xhtml",
            "inner_ctor=SVGRectElement",
            "inner_ns=http://www.w3.org/2000/svg",
            "inner_x=3",
        ]
    );
}

/// `innerHTML` INSIDE a foreign subtree: the fragment-parsing context element now carries the node's
/// real namespace, so the markup is parsed by the foreign-content rules — including for a detached
/// `<clipPath>` whose `<svg>` ancestor is gone, which the ancestor-walk guess could never see.
#[tokio::test]
async fn fragment_parsing_inside_svg_uses_the_nodes_namespace() {
    let out = probe(
        r#"<!doctype html><svg><clipPath id="c"></clipPath></svg>"#,
        r#"
        const cp = document.getElementById("c");
        cp.remove();
        cp.innerHTML = "<rect x='7'/>";
        const r = cp.firstElementChild;
        print("ctor=" + r.constructor.name);
        print("ns=" + r.namespaceURI);
        print("x=" + r.x.baseVal.value);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec!["ctor=SVGRectElement", "ns=http://www.w3.org/2000/svg", "x=7"]
    );
}

/// The XML parser resolves `xmlns` now, so an element's namespace is the one its document DECLARES
/// rather than a default nobody wrote down. Both read sites used to disagree about what "no record"
/// meant: `namespaceURI` answered the XHTML default while `Document.contentType` read the same
/// absence as "no namespace".
#[tokio::test]
async fn xml_documents_resolve_xmlns() {
    let out = probe(
        r#"<!doctype html><div></div>"#,
        r#"
        const p = new DOMParser();
        const x = p.parseFromString("<root><kid/></root>", "application/xml");
        print("bare=" + x.documentElement.namespaceURI);
        const h = p.parseFromString("<html xmlns='http://www.w3.org/1999/xhtml'><body/></html>", "application/xml");
        print("xhtml=" + h.documentElement.namespaceURI);
        print("xhtml_kid=" + h.documentElement.firstElementChild.namespaceURI);
        const s = p.parseFromString("<svg xmlns='http://www.w3.org/2000/svg'><rect/></svg>", "application/xml");
        print("svg=" + s.documentElement.namespaceURI);
        print("svg_rect=" + s.documentElement.firstElementChild.namespaceURI);
        const pre = p.parseFromString("<a:root xmlns:a='urn:x'><a:kid/><plain/></a:root>", "application/xml");
        print("prefixed=" + pre.documentElement.namespaceURI);
        print("prefixed_kid=" + pre.documentElement.firstElementChild.namespaceURI);
        print("plain_kid=" + pre.documentElement.lastElementChild.namespaceURI);
        "#,
    )
    .await;
    assert_eq!(
        out,
        vec![
            "bare=null",
            "xhtml=http://www.w3.org/1999/xhtml",
            "xhtml_kid=http://www.w3.org/1999/xhtml",
            "svg=http://www.w3.org/2000/svg",
            "svg_rect=http://www.w3.org/2000/svg",
            "prefixed=urn:x",
            "prefixed_kid=urn:x",
            "plain_kid=null",
        ]
    );
}
