//! LIVE one-table gate for the ten platform LIST interfaces.
//!
//! Boots the real engine and asserts, for `HTMLCollection`, `HTMLFormControlsCollection`,
//! `HTMLOptionsCollection`, `NodeList`, `RadioNodeList`, `NamedNodeMap`, `DOMTokenList`, `CSSRuleList`,
//! `DOMRectList` and `StyleSheetList`:
//!
//!   1. **Byte-identical prototypes.** `describe_own(Iface.prototype)` is the captured bundle's member
//!      list for that interface, in the capture's order, with the capture's descriptors, followed by the
//!      non-enumerable `constructor` -- the hand-table shape.
//!   2. **The symbols.** Every one owns `Symbol.iterator === Array.prototype.values` (writable,
//!      non-enumerable, configurable) because it has an indexed property getter, and
//!      `Symbol.toStringTag` naming the interface. The three that are `iterable<>` also own
//!      `entries`/`keys`/`values`/`forEach`, and they are `Array.prototype`'s OWN function objects.
//!   3. **The instances.** Every list a page can reach is the right platform type, its own keys are
//!      exactly what Chrome answers (indices, then the supported names -- no symbols, no fields), and
//!      the exotic rules hold: read-only indices, `[LegacyUnenumerableNamedProperties]`, the refusals,
//!      the `"4294967295"` split between the indexed and the named getter, the "Illegal invocation" a
//!      foreign receiver gets, and the WebIDL arity TypeError.
//!   4. **Liveness.** A live collection re-runs its query: `children`, `elements`, `rows` and
//!      `attributes` all see a mutation without being re-read, and a static list does not.
//!
//! Every expected value in section 3 was read from headless Chrome on this machine (2026-09-09); the
//! probe pages are reproduced in the assertions themselves.

use engine::Engine;

const PAGE: &str = r#"<!doctype html><html><head><style>a{color:red}</style></head><body>
  <div id=x class="a b"></div><div>two</div>
  <form id=f><input name=q value=1><input name=q value=2>
    <input type=radio name=r value=a><input type=radio name=r value=b checked>
    <fieldset id=fs><input name=z></fieldset></form>
  <datalist id=dl><option>d1</option></datalist>
  <select id=s><option value=o1>one</option><option value=o2 selected>two</option></select>
  <table id=t><thead><tr><td>h</td></tr></thead><tbody><tr id=r1><td>a</td><th>b</th></tr></tbody></table>
</body></html>"#;

const IFACES: &[&str] = &[
    "HTMLCollection",
    "HTMLFormControlsCollection",
    "HTMLOptionsCollection",
    "NodeList",
    "RadioNodeList",
    "NamedNodeMap",
    "DOMTokenList",
    "CSSRuleList",
    "DOMRectList",
    "StyleSheetList",
];

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();
    e
}

/// The `describe_own` lines the capture's member list implies, with `constructor` at its recorded seat.
fn expected_proto(iface: &str) -> Vec<String> {
    let fx: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(persona::shipped_artifact("surface")).unwrap()).unwrap();
    let members = fx["interfaces"][iface]["members"].as_array().unwrap_or_else(|| panic!("{iface} absent from the bundle surface"));
    let mut out: Vec<String> = members
        .iter()
        .map(|m| {
            let n = m["name"].as_str().unwrap();
            match m["kind"].as_str().unwrap() {
                "accessor" => format!("{n}:accessor:e=1:c=1:s={}", m["set"].as_bool().unwrap() as u8),
                "method" => format!("{n}:data:e=1:c=1:w=1"),
                k => panic!("unexpected kind {k}"),
            }
        })
        .collect();
    // `constructor` AT THE CAPTURE'S SEAT, not appended. It is the last member
    // installed at interface-definition time, so a member a PARTIAL interface adds lands AFTER it --
    // `Navigator` 32 of 66, `Document` 240 of 247, `Screen` 9 of 11 with `onchange` and `isExtended`
    // behind it. Appending it was right on 83 % of the surface and wrong on exactly the interfaces a
    // fingerprinting script reads, and the index has been in the bundle since `3a5f0b60`.
    let at = fx["interfaces"][iface]["ctorIndex"].as_u64().map(|n| n as usize).unwrap_or(out.len()).min(out.len());
    out.insert(at, "constructor:data:e=0:c=1:w=1".to_string());
    out
}

const DESCRIBE: &str = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
    const d = Object.getOwnPropertyDescriptor(o, n);
    return (d.get || d.set)
        ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
        : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
}).join("\n");"#;

#[tokio::test]
async fn every_list_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    for iface in IFACES {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = e.eval_value("coll-desc", src).unwrap().as_str().unwrap().to_string();
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(iface), "{iface}.prototype != the capture");
    }
}

#[tokio::test]
async fn every_list_prototype_carries_the_two_well_known_symbols() {
    let mut e = page().await;
    // Chrome 2026-09-09, for all ten: @@iterator === Array.prototype.values {w:1,e:0,c:1};
    // @@toStringTag is the interface name {w:0,e:0,c:1}.
    for iface in IFACES {
        let src = format!(
            r#"(() => {{
                const P = {iface}.prototype;
                const i = Object.getOwnPropertyDescriptor(P, Symbol.iterator);
                const t = Object.getOwnPropertyDescriptor(P, Symbol.toStringTag);
                return [i.value === Array.prototype.values, +i.writable, +i.enumerable, +i.configurable,
                        t.value, +t.writable, +t.enumerable, +t.configurable].join("|");
            }})()"#
        );
        let got = e.eval_value("coll-sym", src).unwrap().as_str().unwrap().to_string();
        assert_eq!(got, format!("true|1|0|1|{iface}|0|0|1"), "{iface} symbols != Chrome");
    }
    // The iterable<> declarations: NodeList and DOMTokenList (and RadioNodeList through NodeList) hold
    // Array.prototype's OWN function objects -- not approximations of them, the same objects.
    let src = r#"(() => ["NodeList", "DOMTokenList"].map((n) => ["entries","keys","values","forEach"]
        .every((m) => globalThis[n].prototype[m] === Array.prototype[m])).join("|"))()"#;
    let got = e.eval_value("coll-iter", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(got, "true|true");
    // ...and the seven that are NOT iterable<> have none of the four.
    let src = r#"(() => ["HTMLCollection","HTMLFormControlsCollection","HTMLOptionsCollection","NamedNodeMap",
        "CSSRuleList","DOMRectList","StyleSheetList"].every((n) => ["entries","keys","values","forEach"]
        .every((m) => !Object.prototype.hasOwnProperty.call(globalThis[n].prototype, m))))()"#;
    let got = e.eval_value("coll-noniter", src.to_string()).unwrap();
    assert_eq!(got.as_bool(), Some(true), "a non-iterable list interface grew entries/keys/values/forEach");
}

#[tokio::test]
async fn every_list_instance_is_the_platform_type_chrome_answers() {
    let mut e = page().await;
    let src = r#"(() => {
        const T = (v) => Object.prototype.toString.call(v);
        const f = document.getElementById("f"), s = document.getElementById("s"), tb = document.getElementById("t");
        return [
            T(document.querySelectorAll("div")),
            T(document.body.childNodes),
            T(document.getElementsByTagName("div")),
            T(document.getElementsByClassName("a")),
            T(document.getElementsByTagNameNS("*", "div")),
            T(document.body.children),
            T(document.forms), T(document.images), T(document.scripts), T(document.links),
            T(document.embeds), T(document.plugins), T(document.anchors), T(document.applets),
            T(document.getElementsByName("q")),
            T(document.getElementById("x").getElementsByTagName("*")),
            T(f.elements),
            T(document.getElementById("fs").elements),
            T(s.options),
            T(document.getElementById("dl").options),
            T(f.elements.namedItem("r")),
            T(document.getElementById("x").attributes),
            T(document.getElementById("x").classList),
            T(document.styleSheets),
            T(document.styleSheets[0].cssRules),
            T(document.getElementById("x").getClientRects()),
            T(tb.rows), T(tb.tBodies), T(tb.tBodies[0].rows), T(document.getElementById("r1").cells),
            T(document.querySelector("input").labels),
        ].join(",");
    })()"#;
    let got = e.eval_value("coll-types", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(
        got,
        "[object NodeList],[object NodeList],[object HTMLCollection],[object HTMLCollection],[object HTMLCollection],\
[object HTMLCollection],[object HTMLCollection],[object HTMLCollection],[object HTMLCollection],\
[object HTMLCollection],[object HTMLCollection],[object HTMLCollection],[object HTMLCollection],\
[object HTMLCollection],[object NodeList],[object HTMLCollection],[object HTMLFormControlsCollection],\
[object HTMLCollection],[object HTMLOptionsCollection],[object HTMLCollection],[object RadioNodeList],\
[object NamedNodeMap],[object DOMTokenList],[object StyleSheetList],[object CSSRuleList],\
[object DOMRectList],[object HTMLCollection],[object HTMLCollection],[object HTMLCollection],\
[object HTMLCollection],[object NodeList]",
        "a platform list is the wrong interface (Chrome's answers, probed 2026-09-09)"
    );
}

#[tokio::test]
async fn every_list_instance_owns_exactly_the_keys_chrome_owns() {
    let mut e = page().await;
    // Chrome 2026-09-09 on this page: indices, then the supported names in tree order (id first, then
    // an HTML element's name), and NO symbols and no fields of its own.
    let src = r#"(() => {
        const own = (o) => JSON.stringify(Object.getOwnPropertyNames(o));
        const f = document.getElementById("f");
        return [
            own(document.querySelectorAll("div")),
            own(document.getElementsByTagName("div")),
            own(f.elements),
            own(document.getElementById("s").options),
            own(document.getElementById("x").attributes),
            own(document.getElementById("x").classList),
            own(document.createElement("p").classList),
            own(document.styleSheets),
            own(document.styleSheets[0].cssRules),
            own(f.elements.namedItem("r")),
            JSON.stringify(Object.getOwnPropertySymbols(document.querySelectorAll("div")).map(String)),
            JSON.stringify(Object.getOwnPropertySymbols(document.getElementsByTagName("div")).map(String)),
        ].join("\n");
    })()"#;
    let got = e.eval_value("coll-own", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            r#"["0","1"]"#,                               // querySelectorAll("div"): the indices, nothing else
            r#"["0","1","x"]"#,                           // getElementsByTagName: + the id "x"
            r#"["0","1","2","3","4","5","q","r","fs","z"]"#, // form.elements: 6 controls, 4 supported names
            r#"["0","1"]"#,                               // select.options
            r#"["0","1","id","class"]"#,                  // el.attributes: indices then attribute names
            r#"["0","1"]"#,                               // classList of class="a b"
            r#"[]"#,                                      // classList of a class-less element
            r#"["0"]"#,                                   // document.styleSheets
            r#"["0"]"#,                                   // sheet.cssRules
            r#"["0","1"]"#,                               // a RadioNodeList has NO named properties
            r#"[]"#,                                      // no own symbols
            r#"[]"#,
        ],
        "a platform list owns a key Chrome's does not (or is missing one)"
    );
}

#[tokio::test]
async fn the_exotic_indexed_and_named_rules_are_chromes() {
    let mut e = page().await;
    let src = r#"(() => {
        const t = (fn) => { try { return "ok:" + String(fn()); } catch (e) { return "throw:" + e.constructor.name; } };
        const hc = document.getElementsByTagName("div");
        const nl = document.querySelectorAll("div");
        const d0 = Object.getOwnPropertyDescriptor(hc, "0");
        const dx = Object.getOwnPropertyDescriptor(hc, "x");
        return [
            // the descriptors: an index is read-only/enumerable/configurable, a name is
            // [LegacyUnenumerableNamedProperties] -- read-only, NOT enumerable, configurable.
            `${+d0.writable}${+d0.enumerable}${+d0.configurable}`,
            `${+dx.writable}${+dx.enumerable}${+dx.configurable}`,
            // "4294967295" is 2^32-1: NOT an array index, so it reaches the NAMED getter.
            String(hc[4294967295]), String(hc[2147483648]),
            // `in` follows the same split.
            `${0 in hc}${5 in hc}${"x" in hc}${"length" in hc}`,
            // a foreign receiver: WebIDL "Illegal invocation".
            t(() => Object.create(hc).length), t(() => Object.create(nl).item(0)),
            // WebIDL arity.
            t(() => nl.item()), t(() => hc.item()), t(() => hc.namedItem()),
            // namedItem is the NAMED getter invoked directly: "" is never a supported name.
            String(hc.namedItem("")),
            // a supported index and a supported name refuse [[Set]] under "use strict"...
            t(() => { "use strict"; hc[0] = 1; }), t(() => { "use strict"; hc.x = 1; }),
            // ...silently in sloppy mode, leaving the collection untouched.
            t(() => { hc[0] = 1; return Object.prototype.toString.call(hc[0]); }),
            // ...and [[DefineOwnProperty]] on an index or a supported name throws in both modes.
            t(() => Object.defineProperty(hc, "0", { value: 1 })),
            t(() => Object.defineProperty(hc, "x", { value: 1 })),
            // an expando is ordinary, and so is defining `length` (Chrome allows an own one).
            t(() => { hc.zzz = 1; return hc.zzz; }),
            t(() => { Object.defineProperty(hc, "length", { value: 7, configurable: true }); return hc.length; }),
            // deleting a supported index or name is refused; PAST THE END it succeeds.
            t(() => { "use strict"; return delete hc[0]; }),
            t(() => { "use strict"; return delete hc.x; }),
            t(() => { "use strict"; return delete hc[99]; }),
            // the interface object is not newable, and calling it throws.
            t(() => new NodeList()), t(() => NodeList()),
        ].join("|");
    })()"#;
    let got = e.eval_value("coll-exotic", src.to_string()).unwrap().as_str().unwrap().to_string();
    // descriptors (index 011, name 001) | "4294967295" and a past-the-end index | `in` |
    // the two Illegal invocations | three arity TypeErrors | namedItem("") | the two strict refusals |
    // the sloppy no-op | the two defineProperty refusals | the expando | an own `length` |
    // the two delete refusals | delete past the end | new NodeList() | NodeList().
    const EXPECTED: &str = concat!(
        "011|001|undefined|undefined|truefalsetruetrue|",
        "throw:TypeError|throw:TypeError|throw:TypeError|throw:TypeError|throw:TypeError|",
        "null|throw:TypeError|throw:TypeError|ok:[object HTMLDivElement]|",
        "throw:TypeError|throw:TypeError|ok:1|ok:7|",
        "throw:TypeError|throw:TypeError|ok:true|throw:TypeError|throw:TypeError",
    );
    assert_eq!(got, EXPECTED, "an exotic rule diverged from Chrome");
}

#[tokio::test]
async fn a_live_collection_re_runs_its_query_and_a_static_list_does_not() {
    let mut e = page().await;
    let src = r#"(() => {
        const body = document.body, f = document.getElementById("f");
        const kids = body.children, nodes = document.querySelectorAll("div"), childNodes = body.childNodes;
        const elements = f.elements, rows = document.getElementById("t").rows;
        const el = document.getElementById("x"), attrs = el.attributes, cls = el.classList;
        const before = [kids.length, childNodes.length, nodes.length, elements.length, rows.length, attrs.length, cls.length].join(",");
        const d = document.createElement("div");
        body.appendChild(d);
        f.appendChild(document.createElement("input"));
        document.getElementById("t").tBodies[0].appendChild(document.createElement("tr"));
        el.setAttribute("data-new", "1");
        el.classList.add("c");
        const after = [kids.length, childNodes.length, nodes.length, elements.length, rows.length, attrs.length, cls.length].join(",");
        return before + " -> " + after;
    })()"#;
    let got = e.eval_value("coll-live", src.to_string()).unwrap().as_str().unwrap().to_string();
    // Every live count moves; the static NodeList from querySelectorAll does not (it is a snapshot).
    assert_eq!(got, "6,12,2,6,2,2,2 -> 7,13,2,7,3,3,3", "a live collection cached, or a static list did not");
}

#[tokio::test]
async fn wrap_cached_identity_holds_through_every_collection() {
    let mut e = page().await;
    let src = r##"(() => {
        const el = document.getElementById("x");
        const hc = document.getElementsByTagName("div");
        return [
            hc[0] === hc[0],
            hc[0] === el,
            hc.item(0) === el,
            hc.namedItem("x") === el,
            document.body.children[0] === el,
            document.querySelectorAll("#x")[0] === el,
            document.getElementsByClassName("a")[0] === el,
        ].join("|");
    })()"##;
    let got = e.eval_value("coll-ident", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(got, "true|true|true|true|true|true|true", "a collection minted a second wrapper for a node");
}

#[tokio::test]
async fn the_form_and_option_collections_answer_their_own_members() {
    let mut e = page().await;
    let src = r#"(() => {
        const t = (fn) => { try { return "ok:" + String(fn()); } catch (e) { return "throw:" + e.constructor.name + ":" + e.message; } };
        const f = document.getElementById("f"), s = document.getElementById("s");
        return [
            // HTMLFormControlsCollection.namedItem: one match is the element, several is a RadioNodeList.
            Object.prototype.toString.call(f.elements.namedItem("z")),
            Object.prototype.toString.call(f.elements.namedItem("q")),
            Object.prototype.toString.call(f.elements["q"]),
            String(f.elements.namedItem("nope")),
            // RadioNodeList.value reads the checked radio and writes it.
            f.elements.namedItem("r").value,
            t(() => { f.elements.namedItem("r").value = "a"; return f.elements.namedItem("r").value; }),
            // HTMLOptionsCollection: selectedIndex, add, remove, and a WRITABLE length.
            String(s.options.selectedIndex),
            t(() => { const o = document.createElement("option"); o.value = "o3"; s.options.add(o); return s.options.length + "," + s.options[2].value; }),
            t(() => { s.options.remove(2); return s.options.length; }),
            t(() => { s.options.length = 1; return s.options.length + "," + s.children.length; }),
            t(() => { s.options.length = 3; return s.options.length + "," + [...s.children].map((c) => c.tagName).join("/"); }),
        ].join("|");
    })()"#;
    let got = e.eval_value("coll-forms", src.to_string()).unwrap().as_str().unwrap().to_string();
    const EXPECTED: &str = concat!(
        "[object HTMLInputElement]|[object RadioNodeList]|[object RadioNodeList]|null|b|ok:a|1|",
        "ok:3,o3|ok:2|ok:1,1|ok:3,OPTION/OPTION/OPTION",
    );
    assert_eq!(got, EXPECTED, "a form/option collection member diverged from Chrome");
}

#[tokio::test]
async fn the_token_list_and_attribute_map_still_answer_their_own_members() {
    let mut e = page().await;
    let src = r#"(() => {
        const d = document.createElement("div");
        d.className = "a b";
        d.classList.add("c");
        const el = document.getElementById("x");
        const a = el.attributes;
        return [
            d.className, d.classList.contains("b"), d.classList.item(0), d.classList.length,
            Array.from(d.classList.entries(), (x) => x.join("=")).join(";"),
            Array.from(d.classList).join(","), d.classList.toString(), d.classList.value,
            a.length, a[0].name, a.getNamedItem("id").value,
            Object.prototype.toString.call(a.item(0)),
            String(a.getNamedItem("nope")),
        ].join("|");
    })()"#;
    let got = e.eval_value("coll-dtl", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(got, "a b c|true|a|3|0=a;1=b;2=c|a,b,c|a b c|a b c|2|id|x|[object Attr]|null");
}

/// A LIVE LIST SEES `innerHTML`. An earlier change cached every live collection on the
/// DOM's structure revision, and the `innerHTML` setter rewrote the child chain without moving it:
/// `d.innerHTML = "<i>y</i>"` left `d.childNodes[0]` the OLD `<i>x</i>` (and `firstChild`, which does
/// not cache, the new one). lyft.com's markdown renderer parses every card's copy through ONE reused
/// `<template>` and reads `template.content.childNodes`, so every card rendered the first string it
/// parsed and React threw hydration error #418 where Chrome's replay reference throws nothing. The
/// live-collection rule is the DOM's: a `NodeList` from `childNodes` reflects the children as they are.
#[tokio::test]
async fn a_live_list_sees_the_children_inner_html_put_there() {
    let mut e = page().await;
    let src = r#"(() => {
        const out = [];
        const d = document.createElement("div");
        d.innerHTML = "<i>x</i>";
        const kids = d.childNodes;
        out.push(kids[0].textContent);
        d.innerHTML = "<b>y</b><b>z</b>";
        out.push(kids.length + ":" + kids[0].textContent + ":" + d.childNodes[1].textContent);
        d.innerHTML = "";
        out.push(kids.length + ":" + d.children.length);
        const t = document.createElement("template");
        for (const s of ["one", "two", "three"]) {
            t.innerHTML = "<p>" + s + "</p>\n";
            out.push(t.content.childNodes.length + ":" + t.content.childNodes[0].textContent);
        }
        const host = document.getElementById("x");
        const live = host.children;
        host.innerHTML = "<span>a</span><span>b</span>";
        out.push(live.length + ":" + live[1].textContent);
        return out.join("|");
    })()"#;
    let got = e.eval_value("coll-inner-html", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(got, "x|2:y:z|0:0|2:one|2:two|2:three|2:b");
}

/// A `[SameObject]` LIST IS ITS RECEIVER'S REALM'S, whichever realm's getter reads it first. The memoised lists
/// were minted in the getter's realm and kept: an iframe's pristine `Element.prototype` `children` getter called
/// first on a top-level `<div>` (a fingerprinting script's cross-realm read) left `d.children instanceof
/// HTMLCollection` false for good. Every expected value is Chrome 152's on this machine (headless, 2026-10-23).
#[tokio::test]
async fn a_same_object_list_is_made_in_its_receivers_realm() {
    let mut e = page().await;
    let src = r#"(() => {
        const f = document.createElement("iframe");
        document.body.appendChild(f);
        const w = f.contentWindow, wd = w.document;
        const G = (win, i, n) => Object.getOwnPropertyDescriptor(win[i].prototype, n).get;
        const out = [];
        let d = document.createElement("div"), a, b;
        a = G(w, "Element", "children").call(d); b = d.children;
        out.push([a instanceof HTMLCollection, a instanceof w.HTMLCollection, b instanceof HTMLCollection, a === b].join());
        d = document.createElement("div");
        b = d.children; a = G(w, "Element", "children").call(d);
        out.push([a instanceof HTMLCollection, b instanceof HTMLCollection, a === b].join());
        const fe = wd.createElement("div");
        a = G(window, "Element", "children").call(fe);
        out.push([a === fe.children, a instanceof w.HTMLCollection].join());
        a = G(w, "Document", "forms").call(document);
        out.push([a instanceof HTMLCollection, a === document.forms].join());
        const fr = document.createDocumentFragment();
        a = G(w, "DocumentFragment", "children").call(fr);
        out.push([a instanceof HTMLCollection, a === fr.children].join());
        let x = document.createElement("p");
        a = G(w, "Node", "childNodes").call(x);
        out.push([a instanceof NodeList, a === x.childNodes].join());
        x = document.createElement("p");
        a = G(w, "Element", "classList").call(x);
        out.push([a instanceof DOMTokenList, a === x.classList].join());
        // `document.all` and `styleSheets` both ways (Chrome 152, 2026-10-23: true,false,true each, the frame
        // document's false,true,true each; korvus gave the reverse until then)
        a = G(w, "Document", "all").call(document);
        out.push([a instanceof HTMLAllCollection, a instanceof w.HTMLAllCollection, a === document.all].join());
        a = G(w, "Document", "styleSheets").call(document);
        out.push([a instanceof StyleSheetList, a instanceof w.StyleSheetList, a === document.styleSheets].join());
        a = G(window, "Document", "all").call(wd);
        out.push([a instanceof HTMLAllCollection, a instanceof w.HTMLAllCollection, a === wd.all].join());
        a = G(window, "Document", "styleSheets").call(wd);
        out.push([a instanceof StyleSheetList, a instanceof w.StyleSheetList, a === wd.styleSheets].join());
        return out.join("|");
    })()"#;
    let got = e.eval_value("coll-realm", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(
        got,
        "true,false,true,true|true,true,true|true,true|true,true|true,true|true,true|true,true\
         |true,false,true|true,false,true|false,true,true|false,true,true"
    );
}
