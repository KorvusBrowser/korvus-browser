//! Two page-visible defects the post-W5.0b corpus exposed. Both were thrown INSIDE event handlers that
//! the modules used to call by hand (where the exception was swallowed); once the handlers ran through
//! dispatch and `report_exception`, four sites "started throwing" -- the throws were always there.
//!
//!   * `node.getRootNode()` on a page node answered a SECOND wrapper of the arena root, retyped to
//!     `DocumentFragment.prototype`, instead of `document`. React 19's hoistable hydration does
//!     `container.getRootNode()` -> `root.ownerDocument || root` -> `.getElementsByTagName()` and threw
//!     `n.getElementsByTagName is not a function` on react.dev, overreacted.io and a production site.
//!     Chrome: `document.body.getRootNode() === document` is `true` (DOM `get the root`).
//!   * `IDBDatabase.objectStoreNames` (and `IDBTransaction.objectStoreNames`, `IDBObjectStore.indexNames`)
//!     was a plain Array; WebIDL says `DOMStringList` (`length`, `item`, `contains`, an indexed getter and
//!     the value iterator), and a production site's `onupgradeneeded` calls `.contains()` on it.

use engine::Engine;

async fn page(html: &str) -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(html).unwrap();
    e.run().await.unwrap();
    e
}

#[tokio::test]
async fn get_root_node_of_a_page_node_is_the_document_itself() {
    let mut e = page(r#"<!doctype html><html><head></head><body><div id=x><p>t</p></div></body></html>"#).await;
    let src = r#"(() => {
        const x = document.getElementById("x");
        const detached = document.createElement("div");
        const host = document.createElement("div"); document.body.appendChild(host); const sr = host.attachShadow({ mode: "open" });
        const inner = document.createElement("span"); sr.appendChild(inner);
        return [
            x.getRootNode() === document,
            document.getRootNode() === document,
            document.documentElement.getRootNode() === document,
            x.firstChild.firstChild.getRootNode() === document,
            typeof x.getRootNode().getElementsByTagName,
            Object.prototype.toString.call(x.getRootNode()),
            detached.getRootNode() === detached,
            inner.getRootNode() === sr,
            inner.getRootNode({ composed: true }) === document,
        ].join("|");
    })()"#;
    let got = e.eval_value("root-node", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(got, "true|true|true|true|function|[object HTMLDocument]|true|true|true");
}

/// A CREATED document's root element names THAT document as its parent -- on every construction path,
/// and with the document's own identity, not a look-alike.
///
/// The defect this pins: nothing interned the Document object for its arena root, so the first upward
/// walk minted a SECOND object for that node -- a `JsElement` retyped through the nodeType-9 entry of
/// `NODE_TYPE_IFACES`, which is `DocumentFragment`. `parentNode`/`getRootNode` therefore answered an
/// object with no Document member on it, for every document except the page's (which had two
/// hand-written escapes of its own, now deleted).
///
/// EVERY EXPECTATION BELOW IS HEADLESS CHROME 150.0.7871.46, `--headless=new`, measured 2026-09-12 --
/// including the two that are NOT `=== doc`: `new Document()` has no document element at all, and a
/// `<template>`'s content and a shadow root are not documents and must keep answering `null`.
#[tokio::test]
async fn a_created_documents_root_element_names_that_document_as_its_parent() {
    let mut e = page(r#"<!doctype html><html><head></head><body><div id=x></div></body></html>"#).await;
    let src = r#"(() => {
        const t = document.createElement("template"); t.innerHTML = "<p>t</p>";
        const host = document.createElement("div"); const sr = host.attachShadow({ mode: "open" });
        const kinds = [
            ["createHTMLDocument", document.implementation.createHTMLDocument("t"), "HTMLDocument"],
            ["createDocument", document.implementation.createDocument("http://www.w3.org/1999/xhtml", "html", null), "XMLDocument"],
            ["parseFromString/html", new DOMParser().parseFromString("<html><body><p>y</p></body></html>", "text/html"), "HTMLDocument"],
            ["parseFromString/xml", new DOMParser().parseFromString("<r><c/></r>", "text/xml"), "XMLDocument"],
            ["parseHTMLUnsafe", Document.parseHTMLUnsafe("<p>z</p>"), "HTMLDocument"],
            ["page", document, "HTMLDocument"],
        ];
        const out = [];
        for (const [name, d, ctor] of kinds) {
            const p = d.documentElement.parentNode;
            out.push([
                name,
                p === d,                                     // the identity, not a look-alike
                p.constructor.name === ctor,                 // its own interface, not DocumentFragment
                p.nodeType === 9,
                d.documentElement.getRootNode() === d,       // the other upward walk
                d.getRootNode() === d,                       // ... with the DOCUMENT as the receiver
                d.contains(d.documentElement),
                d.compareDocumentPosition(d.documentElement) === 20,
                // `location` is `[LegacyUnforgeable]` on EVERY document, not only the page's: the own
                // accessor is `{ get, set, enumerable: true, configurable: false }` (Chrome 150).
                Object.getOwnPropertyNames(d).join(",") === "location",
                (function (x) { return [typeof x.get, typeof x.set, x.enumerable, x.configurable].join(","); })(
                    Object.getOwnPropertyDescriptor(d, "location")) === "function,function,true,false",
            ].join(","));
        }
        // Not documents, and they must stay that way: `new Document()` is empty (no document element),
        // a template's content and a shadow root have no parent at all.
        out.push(["notDocs",
            new Document().documentElement === null,
            t.content.parentNode === null,
            t.content.constructor.name === "DocumentFragment",
            sr.parentNode === null,
            sr.constructor.name === "ShadowRoot",
            sr.getRootNode() === sr,
        ].join(","));
        return out.join("|");
    })()"#;
    let got = e.eval_value("created-doc-identity", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(
        got,
        "createHTMLDocument,true,true,true,true,true,true,true,true,true\
         |createDocument,true,true,true,true,true,true,true,true,true\
         |parseFromString/html,true,true,true,true,true,true,true,true,true\
         |parseFromString/xml,true,true,true,true,true,true,true,true,true\
         |parseHTMLUnsafe,true,true,true,true,true,true,true,true,true\
         |page,true,true,true,true,true,true,true,true,true\
         |notDocs,true,true,true,true,true,true"
    );
}

#[tokio::test]
async fn object_store_names_is_a_dom_string_list() {
    let mut e = page(r#"<!doctype html><html><body><script>
      globalThis.__R = "pending";
      const req = indexedDB.open("korvus-root-node-live", 1);
      req.onupgradeneeded = (ev) => {
        const db = ev.target.result;
        db.createObjectStore("s");
        const n = db.objectStoreNames;
        const tx = ev.target.transaction;
        globalThis.__R = [
          Object.prototype.toString.call(n), n instanceof DOMStringList,
          n.length, n.item(0), n[0], String(n.item(1)), n.contains("s"), n.contains("zz"),
          Object.keys(n).join(","), n[Symbol.iterator] === Array.prototype.values, Array.from(n).join(","),
          Object.prototype.toString.call(tx.objectStoreNames), tx.objectStoreNames.contains("s"),
          (() => { try { n.contains(); return "no throw"; } catch (e) { return e.name; } })(),
        ].join("|");
      };
      req.onerror = () => { globalThis.__R = "error:" + String(req.error); };
    </script></body></html>"#).await;
    let got = e.eval_value("dsl", "globalThis.__R".to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(got, "[object DOMStringList]|true|1|s|s|null|true|false|0|true|s|[object DOMStringList]|true|TypeError");
}

#[tokio::test]
async fn a_document_is_a_legal_receiver_for_the_mutation_methods() {
    // `unwrap_id` answers only for a `JsElement`, so every mutation method called on a Document was a
    // SILENT no-op once W5.0e deleted the JS facade that used to forward them to the arena root
    // (`05-xml.js`: `appendChild: node => root.appendChild(node)`). WPT
    // `dom/nodes/Node-properties.html` measures exactly this through `foreignDoc.childNodes`: it fell
    // from 703/726 to 686/726, and dom/nodes from 4165 to 4152 of 4225, until `mutation_recv_id`
    // taught the seven callbacks that a Document names its arena root.
    let mut e = page(r#"<!doctype html><html><body></body></html>"#).await;
    let src = r#"(() => {
        const f = document.implementation.createHTMLDocument("");
        const before = f.childNodes.length;
        const c = f.createComment("cmt");
        f.appendChild(c);
        const p = f.createElement("p");
        f.body.appendChild(p);
        const removed = (() => { f.removeChild(c); return f.childNodes.length; })();
        return [
            before,                                   // doctype + html
            f.childNodes.length === before,           // the comment came back off
            c.parentNode === null,
            removed === before,
            (() => { f.appendChild(c); return [f.childNodes.length, f.lastChild === c, f.contains(c)].join(","); })(),
            f.body.contains(p),
            // `c.parentNode === f` was the gap this test used to RECORD instead of assert: it was false
            // here and false before W5.0e too (the deleted facade forwarded `appendChild` to the ROOT
            // ELEMENT, so the parent answered was that element), and the note said identity needed
            // "a per-root document cache -- the same shape `NodeCache` gives elements". That is exactly
            // what W9.18b built: every Document interns itself in `NodeCache` under its arena root
            // (`core/dom/document.rs::intern_document`), so it is asserted now, for a created document
            // and for the page's alike.
            c.parentNode === f,
            document.documentElement.parentNode === document,
        ].join("|");
    })()"#;
    let got = e.eval_value("doc-mutation", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(got, "2|true|true|true|3,true,true|true|true|true");
}

#[tokio::test]
async fn a_shadow_root_and_a_template_fragment_are_not_documents() {
    // Both reuse the arena's document node kind (`09-postshells.js` registers `DocumentFragment.prototype`
    // for nodeType 9), so asking the ARENA "is this a document?" said yes for them and the Document
    // insertion rules refused their second element child. github.com hit it on the first corpus run
    // after the rules landed: `HierarchyRequestError: Only one element on document allowed`. The flag
    // comes from the RECEIVER now (`mutation_recv`), which is exact.
    let mut e = page(r#"<!doctype html><html><body><div id=h></div><template id=t></template></body></html>"#).await;
    let src = r#"(() => {
        const sr = document.getElementById("h").attachShadow({ mode: "open" });
        sr.appendChild(document.createElement("span"));
        sr.appendChild(document.createElement("b"));
        sr.append(document.createElement("i"), document.createTextNode("txt"));
        const c = document.getElementById("t").content;
        c.appendChild(document.createElement("p"));
        c.appendChild(document.createElement("q"));
        c.replaceChildren(document.createElement("u"), document.createTextNode("x"));
        const doc = document.implementation.createHTMLDocument("");
        let threw = "";
        try { doc.appendChild(doc.createElement("div")); } catch (err) { threw = err.name; }
        let threwText = "";
        try { doc.appendChild(doc.createTextNode("t")); } catch (err) { threwText = err.name; }
        return [sr.childNodes.length, c.childNodes.length, threw, threwText, doc.childNodes.length].join("|");
    })()"#;
    let got = e.eval_value("shadow-not-doc", src.to_string()).unwrap().as_str().unwrap().to_string();
    // 4 = span, b, i, text · 2 = u, text · the document refuses a SECOND element and any text · still 2
    assert_eq!(got, "4|2|HierarchyRequestError|HierarchyRequestError|2");
}
