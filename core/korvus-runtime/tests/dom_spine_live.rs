//! LIVE one-table gate for the DOM inheritance spine.
//!
//! Boots the real engine -- whose bootstrap builds `Node`, `Element`, `HTMLElement`, `CharacterData`,
//! `Text`, `Comment`, `CDATASection`, `ProcessingInstruction`, `DocumentType`, `DocumentFragment`,
//! `ShadowRoot`, `Document`, `Attr`, `TreeWalker`, `NodeIterator`, `NodeFilter`, `ElementInternals` and
//! `CustomStateSet` from their `interface!` tables through THE installer (`core/dom/spine.rs`), and fills
//! the wrapper-retype maps from `korvus_dom::tags` (`install/dom_chain.rs::fill_dom_protos`) -- and
//! asserts the RUNNING surface is:
//!   1. BYTE-identical prototypes: `describe_own(X.prototype)` is the captured bundle's member list for
//!      every spine interface, in the capture's order, with the capture's descriptors, `constructor` last.
//!   2. The interface objects: the capture's statics (Node's 18 constants, Document's two parsers), the
//!      constructor chain, and the prototype chain the capture records.
//!   3. Newability as Chrome answers it: `Document`, `Text`, `Comment`, `DocumentFragment` construct;
//!      every other spine interface object is an "Illegal constructor"; `NodeFilter` is a callback
//!      interface (a function with no `prototype` and the sixteen constants; `new` is "not a
//!      constructor").
//!   4. `document.createElement(tag).constructor.name` for EVERY tag in `korvus_dom::tags::HTML_TAGS`,
//!      `createElementNS(svg, tag)` for every `SVG_TAGS` entry and `createElementNS(mathml, tag)` for
//!      every `MATHML_TAGS` entry, the custom-element-name and unknown-tag rules, each foreign
//!      namespace's MISS path -- and every interface the tag tables name is a constructor on the
//!      global, which is the condition under which `fill_dom_protos(...).missing` is empty.
//!   5. Wrapper identity across queries, and the retype of text / comment / doctype / processing
//!      instruction / fragment / shadow root / attribute / walker / iterator / internals wrappers onto
//!      their own prototypes.
//!
//! Every expected value was read from headless Chrome on this machine (2026-09-10; probes p2..p4 in the
//! unit's scratch) or from the bundle's recorded surface.

use engine::Engine;

const SPINE: &[&str] = &[
    "Node",
    "Element",
    "HTMLElement",
    "CharacterData",
    "Text",
    "Comment",
    "CDATASection",
    "ProcessingInstruction",
    "DocumentType",
    "DocumentFragment",
    "ShadowRoot",
    "Document",
    "Attr",
    "TreeWalker",
    "NodeIterator",
    "ElementInternals",
    "CustomStateSet",
    "HTMLUnknownElement",
];

const PAGE: &str = r#"<!doctype html><html><head><title>spine</title></head><body>
  <div id="x" class="a b"><span id="s">one</span>two<!--c--><p id="p">three</p></div>
</body></html>"#;

fn surface() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(persona::shipped_artifact("surface")).unwrap()).unwrap()
}

/// The `describe_own` lines the capture's member list implies (a constant is
/// `{ writable: false, enumerable: true, configurable: false }`), with `constructor` at its recorded seat.
fn expected_proto(fx: &serde_json::Value, iface: &str) -> Vec<String> {
    let members = fx["interfaces"][iface]["members"].as_array().unwrap_or_else(|| panic!("{iface} absent from the bundle surface"));
    let mut out: Vec<String> = members.iter().map(|m| describe_line(m)).collect();
    // `constructor` AT THE CAPTURE'S SEAT, not appended. It is the last member
    // installed at interface-definition time, so a member a PARTIAL interface adds lands AFTER it --
    // `Navigator` 32 of 66, `Document` 240 of 247, `Screen` 9 of 11 with `onchange` and `isExtended`
    // behind it. Appending it was right on 83 % of the surface and wrong on exactly the interfaces a
    // fingerprinting script reads, and the index has been in the bundle since `3a5f0b60`.
    let at = fx["interfaces"][iface]["ctorIndex"].as_u64().map(|n| n as usize).unwrap_or(out.len()).min(out.len());
    out.insert(at, "constructor:data:e=0:c=1:w=1".to_string());
    out
}

fn describe_line(m: &serde_json::Value) -> String {
    let n = m["name"].as_str().unwrap();
    match m["kind"].as_str().unwrap() {
        "accessor" => format!("{n}:accessor:e=1:c=1:s={}", m["set"].as_bool().unwrap_or(false) as u8),
        "method" => format!("{n}:data:e=1:c=1:w=1"),
        "value" => format!("{n}:data:e={}:c={}:w=0", m["enumerable"].as_bool().unwrap_or(true) as u8, m["configurable"].as_bool().unwrap_or(false) as u8),
        k => panic!("unexpected member kind {k}"),
    }
}

const DESCRIBE: &str = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
    const d = Object.getOwnPropertyDescriptor(o, n);
    return (d.get || d.set)
        ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
        : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
}).join("\n");"#;

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();
    e
}

fn eval_str(e: &mut Engine, name: &'static str, src: String) -> String {
    let v = e.eval_value(name, src).unwrap();
    v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string())
}

// ── 1. the prototypes ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_spine_prototype_is_byte_identical_to_the_capture() {
    let fx = surface();
    let mut e = page().await;
    for iface in SPINE {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = eval_str(&mut e, "spine-desc", src);
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(&fx, iface), "{iface}.prototype != the capture");
    }
}

#[tokio::test]
async fn every_spine_prototype_carries_its_string_tag_and_nothing_else_symbolic_leaks_to_a_page() {
    let mut e = page().await;
    for iface in SPINE {
        let src = format!(
            r#"(() => {{
                const t = Object.getOwnPropertyDescriptor({iface}.prototype, Symbol.toStringTag);
                return [t.value, +t.writable, +t.enumerable, +t.configurable].join("|");
            }})()"#
        );
        assert_eq!(eval_str(&mut e, "spine-tag", src), format!("{iface}|0|0|1"), "{iface} @@toStringTag");
    }
    // CustomStateSet is setlike: @@iterator IS `values` (Chrome: {w:1,e:0,c:1}) and its iterator is
    // "[object CustomStateSet Iterator]".
    let src = r#"(() => {
        const P = CustomStateSet.prototype;
        const d = Object.getOwnPropertyDescriptor(P, Symbol.iterator);
        return [d.value === P.values, +d.writable, +d.enumerable, +d.configurable].join("|");
    })()"#;
    assert_eq!(eval_str(&mut e, "spine-css-iter", src.to_string()), "true|1|0|1");
}

// ── 2. the interface objects and the chains ─────────────────────────────────────────────────────

#[tokio::test]
async fn the_interface_objects_carry_the_captures_statics_and_chains() {
    let fx = surface();
    let mut e = page().await;
    for iface in SPINE {
        let d = &fx["interfaces"][iface];
        // Own names after the function's own `length`, `name`, `prototype` are the statics, in order.
        let statics: Vec<String> = d["statics"].as_array().unwrap().iter().map(describe_line).collect();
        let src = format!(
            r#"(() => {{ {DESCRIBE} const all = desc({iface}).split("\n").filter((l) => l.length);
                const own = Object.getOwnPropertyNames({iface});
                return JSON.stringify({{ head: own.slice(0, 3), statics: all.slice(3), len: {iface}.length, name: {iface}.name, ty: typeof {iface} }}); }})()"#
        );
        let got: serde_json::Value = serde_json::from_str(&eval_str(&mut e, "spine-statics", src)).unwrap();
        assert_eq!(got["head"], serde_json::json!(["length", "name", "prototype"]), "{iface} own head");
        assert_eq!(got["statics"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect::<Vec<_>>(), statics, "{iface} statics");
        assert_eq!(got["len"], d["length"], "{iface}.length");
        assert_eq!(got["name"].as_str(), Some(*iface), "{iface}.name");
        assert_eq!(got["ty"].as_str(), Some("function"));
        // The prototype chain the capture records (`proto`), and the constructor chain beside it.
        let chain: Vec<String> = d["proto"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        let src = format!(
            r#"(() => {{
                const names = [];
                for (let p = Object.getPrototypeOf({iface}.prototype); p && p !== Object.prototype; p = Object.getPrototypeOf(p)) names.push(p.constructor.name);
                const parent = names.length ? globalThis[names[0]] : Function.prototype;
                return JSON.stringify({{ names, ctorChain: Object.getPrototypeOf({iface}) === parent }});
            }})()"#
        );
        let got: serde_json::Value = serde_json::from_str(&eval_str(&mut e, "spine-chain", src)).unwrap();
        assert_eq!(got["names"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect::<Vec<_>>(), chain, "{iface} prototype chain");
        assert_eq!(got["ctorChain"], serde_json::json!(true), "{iface} constructor chain");
    }
    // Node's constants sit on BOTH holders with the same value and the constant descriptor.
    let src = r#"(() => ["ELEMENT_NODE", "DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC"].map((k) => {
        const a = Object.getOwnPropertyDescriptor(Node, k), b = Object.getOwnPropertyDescriptor(Node.prototype, k);
        return [a.value, b.value, +a.writable, +a.enumerable, +a.configurable, +b.writable, +b.enumerable, +b.configurable].join(",");
    }).join("|"))()"#;
    assert_eq!(eval_str(&mut e, "spine-consts", src.to_string()), "1,1,0,1,0,0,1,0|32,32,0,1,0,0,1,0");
}

// ── 3. newability ───────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn newability_is_chromes() {
    let fx = surface();
    let mut e = page().await;
    for iface in SPINE {
        let newable = fx["interfaces"][iface]["newable"].as_bool().unwrap();
        let src = format!(
            r#"(() => {{
                const t = (f) => {{ try {{ const v = f(); return "ok:" + Object.prototype.toString.call(v); }} catch (e) {{ return e.constructor.name + ":" + e.message; }} }};
                return [t(() => new {iface}()), t(() => {iface}())].join("\n");
            }})()"#
        );
        let got = eval_str(&mut e, "spine-new", src);
        let lines: Vec<&str> = got.lines().collect();
        if newable {
            assert_eq!(lines[0], format!("ok:[object {iface}]"), "new {iface}() must construct");
            assert_eq!(
                lines[1],
                format!("TypeError:Failed to construct '{iface}': Please use the 'new' operator, this DOM object constructor cannot be called as a function."),
                "{iface}() without new"
            );
        } else {
            // EXACT, both shapes. These two lines were `starts_with("TypeError:") &&
            // contains("Illegal constructor")`, which is true of Chrome's message AND of the
            // unprefixed one this engine used to throw -- the weakening that let the gap sit.
            // Measured, Chrome 150 over loopback, 2026-09-12.
            assert_eq!(lines[0], format!("TypeError:Failed to construct '{iface}': Illegal constructor"), "new {iface}()");
            // `HTMLElement` is the ONE `[HTMLConstructor]` interface of the spine: WebIDL gives it a
            // constructor (that is how a custom element is upgraded), so a bare call is "you forgot
            // new" and not "there is nothing to construct". Every other spine interface declares none.
            let want_call = if *iface == "HTMLElement" {
                format!("TypeError:Failed to construct '{iface}': Please use the 'new' operator, this DOM object constructor cannot be called as a function.")
            } else {
                "TypeError:Illegal constructor".to_string()
            };
            assert_eq!(lines[1], want_call, "{iface}() without new");
        }
    }
    // `new Document()` is an EMPTY XML document with no browsing context (Chrome, 2026-09-10).
    let src = r#"(() => { const d = new Document(); return [d.nodeType, d.contentType, d.documentElement, d.defaultView, d.hidden, d.visibilityState, d.hasFocus(), d.location, Object.getOwnPropertyNames(d).join(","), d.xmlVersion, d.webkitHidden].join("|"); })()"#;
    // (`Array.prototype.join` renders a null element as the empty string.)
    assert_eq!(eval_str(&mut e, "spine-newdoc", src.to_string()), "9|application/xml|||true|hidden|false||location|1.0|true");
    // The page document: `location` is its ONE own property, an unforgeable accessor.
    let src = r#"(() => { const d = Object.getOwnPropertyDescriptor(document, "location"); return [Object.getOwnPropertyNames(document).join(","), typeof d.get, typeof d.set, +d.enumerable, +d.configurable, document.location === location, document.defaultView === globalThis, document.xmlVersion].join("|"); })()"#;
    assert_eq!(eval_str(&mut e, "spine-doc-own", src.to_string()), "location|function|function|1|0|true|true|");
    // NodeFilter: a callback interface -- a function, no prototype, the sixteen constants as own
    // properties, `new` "is not a constructor", a call "Illegal constructor" (Chrome, 2026-09-10).
    let src = r#"(() => {
        const t = (f) => { try { f(); return "ok"; } catch (e) { return e.constructor.name + ":" + e.message; } };
        const d = Object.getOwnPropertyDescriptor(NodeFilter, "SHOW_ALL");
        return [typeof NodeFilter, String(NodeFilter.prototype), Object.getOwnPropertyNames(NodeFilter).join(","),
                NodeFilter.FILTER_ACCEPT, NodeFilter.SHOW_ALL, +d.writable, +d.enumerable, +d.configurable,
                t(() => new NodeFilter()), t(() => NodeFilter()), NodeFilter.name, NodeFilter.length].join("|");
    })()"#;
    assert_eq!(
        eval_str(&mut e, "spine-nodefilter", src.to_string()),
        "function|undefined|length,name,FILTER_ACCEPT,FILTER_REJECT,FILTER_SKIP,SHOW_ALL,SHOW_ELEMENT,SHOW_ATTRIBUTE,SHOW_TEXT,SHOW_CDATA_SECTION,SHOW_ENTITY_REFERENCE,SHOW_ENTITY,SHOW_PROCESSING_INSTRUCTION,SHOW_COMMENT,SHOW_DOCUMENT,SHOW_DOCUMENT_TYPE,SHOW_DOCUMENT_FRAGMENT,SHOW_NOTATION|1|4294967295|0|1|0|TypeError:NodeFilter is not a constructor|TypeError:Illegal constructor|NodeFilter|0"
    );
}

// ── 4. the tag tables ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_tag_in_the_table_makes_the_interface_the_table_names() {
    let mut e = page().await;
    let html: Vec<String> = webapi::dom::tags::HTML_TAGS.iter().map(|(t, i)| format!("[{:?},{:?}]", t, i)).collect();
    let svg: Vec<String> = webapi::dom::tags::SVG_TAGS.iter().map(|(t, i)| format!("[{:?},{:?}]", t, i)).collect();
    let mml: Vec<String> = webapi::dom::tags::MATHML_TAGS.iter().map(|(t, i)| format!("[{:?},{:?}]", t, i)).collect();
    // 142 with `fencedframe`, whose interface the Chrome capture exposes (Brave 151 hid it), and Chrome 152's
    // `usermedia` (`HTMLUserMediaElement`, measured: `document.createElement('usermedia')` is one).
    assert_eq!(webapi::dom::tags::HTML_TAGS.len(), 142, "the HTML tag table");
    let src = format!(
        r#"(() => {{
            const bad = [];
            for (const [tag, iface] of [{}]) {{
                const el = document.createElement(tag);
                const got = el.constructor.name;
                if (got !== iface || Object.getPrototypeOf(el) !== globalThis[iface].prototype) bad.push(tag + "->" + got + " (table: " + iface + ")");
            }}
            for (const [tag, iface] of [{}]) {{
                const el = document.createElementNS("http://www.w3.org/2000/svg", tag);
                const got = el.constructor.name;
                if (got !== iface || Object.getPrototypeOf(el) !== globalThis[iface].prototype) bad.push("svg:" + tag + "->" + got + " (table: " + iface + ")");
            }}
            for (const [tag, iface] of [{}]) {{
                const el = document.createElementNS("http://www.w3.org/1998/Math/MathML", tag);
                const got = el.constructor.name;
                if (got !== iface || Object.getPrototypeOf(el) !== globalThis[iface].prototype) bad.push("mathml:" + tag + "->" + got + " (table: " + iface + ")");
            }}
            return JSON.stringify(bad);
        }})()"#,
        html.join(","),
        svg.join(","),
        mml.join(",")
    );
    assert_eq!(eval_str(&mut e, "spine-tags", src), "[]", "a tag reaches the page on the wrong prototype");
    // The MISS path of each FOREIGN namespace is that namespace's `fallbackJSInterfaceName`, not
    // `HTMLUnknownElement`. Headless Chrome 150 on this machine, measured 2026-09-12:
    // `createElementNS(mathmlNS, "zzznotatag").constructor.name` is "MathMLElement", and inline
    // `<math><mi>` in markup is a MathMLElement too. Until W9.1 every MathML element on every page was
    // an `HTMLUnknownElement` -- `MATHML_TAGS` was generated and read by nothing.
    let src = r##"(() => {
        const M = "http://www.w3.org/1998/Math/MathML";
        const d = document.createElement("div");
        d.innerHTML = "<math><mi id='mi'>x</mi><annotation-xml id='ax'></annotation-xml></math>";
        return [
            document.createElementNS(M, "zzznotatag").constructor.name,
            document.createElementNS(M, "mi").namespaceURI,
            d.querySelector("#mi").constructor.name,
            d.querySelector("#ax").constructor.name,
            d.querySelector("#ax").localName,
            document.createElement("mi").constructor.name,
        ].join("|");
    })()"##;
    assert_eq!(
        eval_str(&mut e, "spine-mathml-miss", src.to_string()),
        "MathMLElement|http://www.w3.org/1998/Math/MathML|MathMLElement|MathMLElement|annotation-xml|HTMLUnknownElement"
    );
    // The two rules around the table (Chrome, 2026-09-10): a valid custom element name is a plain
    // HTMLElement even before a definition exists; anything else unknown is HTMLUnknownElement;
    // `plaintext` is HTMLElement and `xmp` HTMLPreElement; the case-folding.
    let src = r#"(() => ["foo-bar", "FOO-BAR", "zzz", "Foo", "image", "applet", "plaintext", "xmp", "DIV"].map((t) => document.createElement(t).constructor.name).join(","))()"#;
    assert_eq!(
        eval_str(&mut e, "spine-tag-rules", src.to_string()),
        "HTMLElement,HTMLElement,HTMLUnknownElement,HTMLUnknownElement,HTMLUnknownElement,HTMLUnknownElement,HTMLElement,HTMLPreElement,HTMLDivElement"
    );
}

/// The condition under which `install::dom_chain::fill_dom_protos(...).missing` is empty: every interface
/// the tag tables (and the per-nodeType list) name is a constructor on the global with a prototype.
#[tokio::test]
async fn every_interface_the_tag_tables_name_is_on_the_global() {
    let mut e = page().await;
    let mut names: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for (_, i) in webapi::dom::tags::HTML_TAGS { names.insert(i); }
    for (_, i) in webapi::dom::tags::SVG_TAGS { names.insert(i); }
    for (_, i) in webapi::dom::tags::MATHML_TAGS { names.insert(i); }
    for (_, i) in webapi::install::dom_chain::NODE_TYPE_IFACES { names.insert(i); }
    // The three MISS paths `fill_dom_protos` also resolves: HTML's, and each foreign namespace's
    // `fallbackJSInterfaceName`.
    for i in ["ShadowRoot", "HTMLUnknownElement", "SVGElement", "MathMLElement"] { names.insert(i); }
    let list: Vec<String> = names.iter().map(|n| format!("{n:?}")).collect();
    let src = format!(
        r#"(() => JSON.stringify([{}].filter((n) => typeof globalThis[n] !== "function" || typeof globalThis[n].prototype !== "object")))()"#,
        list.join(",")
    );
    assert_eq!(eval_str(&mut e, "spine-missing", src), "[]", "fill_dom_protos would report these as missing");
    assert_eq!(webapi::install::dom_chain::NODE_TYPE_IFACES.iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec![3, 4, 7, 8, 9, 10, 11]);
}

// ── 5. identity and the retypes ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn wrappers_are_canonical_and_every_node_kind_lands_on_its_own_prototype() {
    let mut e = page().await;
    let src = r##"(() => {
        const x = document.getElementById("x");
        const same = [
            x === document.querySelector("#x"),
            x === document.body.firstElementChild,
            x === document.body.children[0],
            x === document.body.childNodes[1],
            x === document.getElementsByTagName("div")[0],
            x === document.querySelectorAll("div")[0],
            x.firstElementChild === x.childNodes[0],
            x.firstElementChild.parentNode === x,
            x.childNodes[1] === x.firstChild.nextSibling,
            document.documentElement.parentNode === document,
            document.doctype === document.firstChild,
            document.doctype.parentNode === document,
            document.body.ownerDocument === document,
            x.getRootNode() === document,
        ];
        const P = (v) => Object.getPrototypeOf(v);
        const t = x.childNodes[1], c = x.childNodes[2];
        const pi = document.createProcessingInstruction("xml-stylesheet", "href=x");
        const frag = document.createDocumentFragment();
        const sr = document.createElement("div").attachShadow({ mode: "open" });
        const attr = x.getAttributeNode("id");
        const tw = document.createTreeWalker(document.body), ni = document.createNodeIterator(document.body);
        customElements.define("x-probe", class extends HTMLElement {});
        const ce = document.createElement("x-probe");
        const ei = ce.attachInternals();
        const types = {
            text: t.nodeType === 3 && P(t) === Text.prototype && t instanceof Text && t instanceof CharacterData && t instanceof Node && !(t instanceof Element),
            comment: c.nodeType === 8 && P(c) === Comment.prototype && c instanceof Comment && c instanceof CharacterData,
            doctype: document.doctype.nodeType === 10 && P(document.doctype) === DocumentType.prototype && document.doctype instanceof DocumentType && document.doctype.name === "html",
            pi: pi.nodeType === 7 && P(pi) === ProcessingInstruction.prototype && pi.target === "xml-stylesheet" && pi.data === "href=x",
            fragment: frag.nodeType === 11 && P(frag) === DocumentFragment.prototype && P(new DocumentFragment()) === DocumentFragment.prototype,
            shadow: P(sr) === ShadowRoot.prototype && sr instanceof DocumentFragment && sr.host.shadowRoot === sr,
            ctors: P(new Text("a")) === Text.prototype && P(new Comment("b")) === Comment.prototype,
            // The page document is an HTMLDocument (Chrome: `Object.getPrototypeOf(document) ===
            // HTMLDocument.prototype`), chained through Document.prototype; `new Document()` is a Document.
            document: P(document) === HTMLDocument.prototype && P(HTMLDocument.prototype) === Document.prototype && P(new Document()) === Document.prototype && document instanceof Document,
            attrNodeType: attr.nodeType === 2,
            attrProto: P(attr) === Attr.prototype && attr instanceof Node,
            attrValue: attr.value === "x" && attr.ownerElement === x && attr.name === "id",
            walkers: P(tw) === TreeWalker.prototype && P(ni) === NodeIterator.prototype && tw.currentNode === document.body,
            internals: P(ei) === ElementInternals.prototype && P(ei.states) === CustomStateSet.prototype && ei.shadowRoot === null,
            elements: P(x) === HTMLDivElement.prototype && P(document.createElement("foo-bar")) === HTMLElement.prototype,
            unknown: P(document.createElement("zzz")) === HTMLUnknownElement.prototype,
            tags: Object.prototype.toString.call(t) === "[object Text]" && Object.prototype.toString.call(document.doctype) === "[object DocumentType]" && Object.prototype.toString.call(pi) === "[object ProcessingInstruction]",
            strings: String(x) === "[object HTMLDivElement]" && String(document) === "[object HTMLDocument]",
            values: x.textContent === "onetwothree" && document.textContent === null && document.nodeName === "#document" && document.nodeValue === null,
        };
        return JSON.stringify({ same, types });
    })()"##;
    let got: serde_json::Value = serde_json::from_str(&eval_str(&mut e, "spine-identity", src.to_string())).unwrap();
    let same: Vec<bool> = got["same"].as_array().unwrap().iter().map(|v| v.as_bool().unwrap()).collect();
    assert_eq!(same, vec![true; same.len()], "a node-returning member handed back a non-canonical wrapper");
    let wrong: Vec<String> = got["types"].as_object().unwrap().iter().filter(|(_, v)| !v.as_bool().unwrap()).map(|(k, _)| k.clone()).collect();
    assert_eq!(wrong, Vec::<String>::new(), "a node kind is on the wrong prototype (or answers wrong)");
}
