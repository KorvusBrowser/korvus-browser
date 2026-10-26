//! innerHTML setter + insertAdjacentHTML (HTML fragment parsing).
use engine::Engine;
#[tokio::test]
async fn innerhtml_and_adjacent() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><div id="d"></div></body></html>"#).unwrap();
    e.eval("f", r##"
        const d = document.getElementById("d");
        d.innerHTML = "<span class='x'>hi</span><b>bold</b>";
        print("set=" + d.childElementCount + " qs=" + d.querySelector(".x").textContent);
        print("inner=" + d.innerHTML);
        d.insertAdjacentHTML("beforeend", "<i>end</i>");
        d.insertAdjacentHTML("afterbegin", "<u>begin</u>");
        print("order=" + Array.from(d.children).map(c => c.tagName).join(","));
    "##.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "set=2 qs=hi",
        "inner=<span class=\"x\">hi</span><b>bold</b>",
        "order=U,SPAN,B,I",
    ]);
}

/// WHATWG "fragment parsing algorithm": `innerHTML=` parses with the target element as CONTEXT, so
/// the parser's insertion mode is the one that element implies. Without it (`parse_document`, no
/// context) "in body" foster-parents a cell's text out of the table and drops every tag, and
/// `table.innerHTML = '<tr><td>x</td></tr>'` reads back `"x"` -- every row and cell destroyed.
///
/// Every expectation here is headless Chrome 151 (`--dump-dom` over loopback, 2026-09-11).
#[tokio::test]
async fn fragment_parsing_uses_the_context_element() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body></body></html>"#).unwrap();
    e.eval("f", r##"
        function inner(tag, html) {
            const el = document.createElement(tag);
            el.innerHTML = html;
            return el.innerHTML;
        }
        print(inner("table", "<tr><td>x</td></tr>"));
        print(inner("tbody", "<tr><td>y</td></tr>"));
        print(inner("tr", "<td>z</td>"));
        print(inner("select", "<option>a</option>"));
        print(inner("ul", "<li>a</li>"));
        print(inner("p", "<b>a</b>"));
        print(inner("template", "<tr><td>t</td></tr>"));
        // insertAdjacentHTML: the element for the two INSIDE positions, its parent for the outside two.
        const t = document.createElement("table");
        const tb = document.createElement("tbody");
        t.appendChild(tb);
        tb.insertAdjacentHTML("beforebegin", "<tr><td>bb</td></tr>");
        print(t.innerHTML);
        const tr = document.createElement("tr");
        tr.insertAdjacentHTML("beforeend", "<td>be</td>");
        print(tr.innerHTML);
        // createContextualFragment: the range's start container.
        const t2 = document.createElement("table");
        const r = document.createRange();
        r.selectNodeContents(t2);
        const hold = document.createElement("div");
        hold.appendChild(r.createContextualFragment("<tr><td>cf</td></tr>"));
        print(hold.innerHTML);
        // outerHTML: the element's PARENT.
        const t3 = document.createElement("table");
        const tb3 = document.createElement("tbody");
        t3.appendChild(tb3);
        tb3.outerHTML = "<tr><td>oh</td></tr>";
        print(t3.innerHTML);
    "##.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "<tbody><tr><td>x</td></tr></tbody>",
        "<tr><td>y</td></tr>",
        "<td>z</td>",
        "<option>a</option>",
        "<li>a</li>",
        "<b>a</b>",
        "<tr><td>t</td></tr>",
        "<tbody><tr><td>bb</td></tr></tbody><tbody></tbody>",
        "<td>be</td>",
        "<tbody><tr><td>cf</td></tr></tbody>",
        "<tbody><tr><td>oh</td></tr></tbody>",
    ]);
}

/// The second symptom of the same cause, and why `httpbin.org` threw: DOMPurify's very first
/// `_initDocument` feature-detect sets `body.outerHTML` on a document whose `<head>` it just
/// removed, then reads `getElementsByTagName('body')[0]`. `outerHTML=` parses with the element's
/// PARENT as context -- here `<html>`, whose "before head" insertion mode synthesizes a fresh
/// `<head>` and `<body>` around the markup. With no context element the SVG simply replaced the
/// body, the detect read `undefined`, and swagger-ui died on `undefined.querySelector`.
///
/// Chrome 151 (`--dump-dom` over loopback, 2026-09-11): `[object HTMLBodyElement]` / `[HEAD, BODY]`.
#[tokio::test]
async fn outer_html_on_body_reparses_in_the_html_context() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body></body></html>"#).unwrap();
    e.eval("f", r##"
        const d = document.implementation.createHTMLDocument("");
        d.head.remove();
        d.body.outerHTML = '<svg><g onload="1"></g></svg>';
        print(String(d.getElementsByTagName("body")[0]));
        print(Array.from(d.documentElement.children).map(e => e.nodeName).join(","));
    "##.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec!["[object HTMLBodyElement]", "HEAD,BODY"]);
}

/// The two clauses of "insert adjacent HTML" that are about picking the context, not about parsing:
/// step 2 (a null or Document parent is a NoModificationAllowedError for the OUTSIDE positions) and
/// step 3 (a non-Element context, or the `<html>` element of an HTML document, becomes a `<body>`).
/// Without step 3 an `<html>` context runs "before head" and every call grows another head+body pair.
///
/// Chrome 151 (`--dump-dom` over loopback, 2026-09-11) is every expectation here.
#[tokio::test]
async fn insert_adjacent_html_picks_the_context_the_spec_names() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body></body></html>"#).unwrap();
    e.eval("f", r##"
        // Step 3: an <html> context becomes <body>, so head/body are NOT re-synthesized.
        const d = document.implementation.createHTMLDocument("");
        d.body.insertAdjacentHTML("afterend", "<p>");
        d.head.insertAdjacentHTML("beforebegin", "<p>");
        print(d.getElementsByTagName("head").length + "," + d.getElementsByTagName("body").length);
        print(Array.from(d.documentElement.children).map(e => e.nodeName).join(","));
        // Step 3: a DocumentFragment parent is not an Element either -> <body> context, and the
        // nodes really are inserted into the fragment.
        const f = document.createDocumentFragment();
        const c = document.createElement("div");
        f.appendChild(c);
        c.insertAdjacentHTML("beforebegin", "<tr><td>q</td></tr>");
        const hold = document.createElement("div");
        hold.appendChild(f.cloneNode(true));
        print(hold.innerHTML);
        // Step 2: no parent, and a Document parent, both throw.
        const orphan = document.createElement("div");
        const err = f2 => { try { f2(); return "no-throw"; } catch (e) { return e.name; } };
        print(err(() => orphan.insertAdjacentHTML("afterend", "")));
        print(err(() => orphan.insertAdjacentHTML("beforebegin", "<div></div>")));
        print(err(() => document.documentElement.insertAdjacentHTML("afterend", "<div></div>")));
        // ... but the INSIDE positions never consult the parent.
        print(err(() => orphan.insertAdjacentHTML("beforeend", "ok")) + ":" + orphan.textContent);
    "##.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "1,1",
        "P,HEAD,BODY,P",
        "q<div></div>",
        "NoModificationAllowedError",
        "NoModificationAllowedError",
        "NoModificationAllowedError",
        "no-throw:ok",
    ]);
}
