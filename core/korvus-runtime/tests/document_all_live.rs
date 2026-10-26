//! LIVE gate for `document.all` — the platform's one `[[IsHTMLDDA]]` object.
//!
//! `Document::all` was the top entry of the project's first parity-scale demand signal
//! (a stub-demand map: 33 of 47 sites, 69 hits) and it answered the literal
//! `null`, which satisfies the falsiness half of `[[IsHTMLDDA]]` by accident and every other half
//! not at all — `typeof` was `"object"`, and `length`, indexed access and named access all THREW.
//!
//! **Every expectation below was read from Google Chrome 150.0.7871.46 on this machine on
//! 2026-09-21**, `--headless --dump-dom` over a `127.0.0.1` server, on the pages the assertions
//! reproduce. Where WPT and Chrome disagree the comment says so and names the subtest: Chrome passes
//! **40 of the 41** subtests of `html/infrastructure/common-dom-interfaces/collections/
//! htmlallcollection.html` — measured, on that file's own bytes — and fails "collections are new
//! live HTMLCollection instances", because Chrome CACHES the collection a repeated name answers and
//! that subtest asserts a fresh one per call.

use engine::Engine;

/// The WPT page's shape, which is also the shape the Chrome readings were taken on: a repeated
/// `name` (two `<a name=foo>`), a `name` on a tag that is NOT "all"-named (`<div name=divwithname>`),
/// ids that look like array indices and ids that only look like them.
const PAGE: &str = r#"<!doctype html><html id="root"><head><title>t</title>
<meta name="flags" content="TOKENS"></head><body id="tags">
<img name="picture"><a name="foo"></a><a name="foo"></a>
<span id="42"></span><span id="043"></span>
<div id="4294967294"></div><div id="4294967295"></div><div id="4294967296"></div>
<div id="undefined"></div><div id="null"></div><div name="divwithname"></div><div id="-0"></div>
<input name="length"><svg id="svgid" name="svgname"></svg>
</body></html>"#;

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();
    e
}

fn eval(e: &mut Engine, tag: &'static str, src: &str) -> String {
    e.eval_value(tag, src.to_string()).unwrap().as_str().unwrap_or_default().to_string()
}

/// `[[IsHTMLDDA]]`, which is the half no JS shim can produce: `typeof` is `"undefined"` because the
/// object carries V8's UNDETECTABLE map bit (`v8::ObjectTemplate::MarkAsUndetectable`), and the
/// loose-equality and ToBoolean answers fall out of the same bit. Chrome 150, measured:
///
/// ```text
///   typeof document.all            undefined      document.all === undefined   false
///   document.all == null           true           document.all === null        false
///   Boolean(document.all)          false          !document.all                true
///   document.all == false          false          document.all == 0            false
///   String(document.all)           [object HTMLAllCollection]
/// ```
///
/// The pair that matters to a page is the first column against the second: a library asking
/// `typeof document.all !== "undefined"` must get `false` (it is asking "am I on IE") while
/// `document.all === undefined` must get `false` too (the object is really there).
#[tokio::test]
async fn document_all_is_undetectable_the_way_chrome_is() {
    let mut e = page().await;
    let src = r#"(() => [
        typeof document.all,
        document.all == null, document.all === null,
        document.all === undefined, document.all == undefined,
        !document.all, Boolean(document.all), (document.all ? "t" : "f"),
        document.all == false, document.all == 0,
        String(document.all), Object.prototype.toString.call(document.all),
        typeof document.all !== "undefined",
        document.all instanceof Object, document.all instanceof Function, Array.isArray(document.all),
        JSON.stringify({x: document.all}),
    ].join("|"))()"#;
    assert_eq!(
        eval(&mut e, "all-dda", src),
        "undefined|true|false|false|true|true|false|f|false|false|\
[object HTMLAllCollection]|[object HTMLAllCollection]|false|true|false|false|{}"
    );
}

/// The prototype is the capture's, member for member, and it is NOT an `HTMLCollection` subclass:
/// `Object.getPrototypeOf(HTMLAllCollection.prototype)` is `Object.prototype` in Chrome.
#[tokio::test]
async fn html_all_collection_prototype_is_the_captures() {
    let mut e = page().await;
    let fx: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(persona::shipped_artifact("surface")).unwrap()).unwrap();
    let iface = &fx["interfaces"]["HTMLAllCollection"];
    let mut want: Vec<String> = iface["members"]
        .as_array()
        .expect("HTMLAllCollection absent from the bundle surface")
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
    // `constructor` at the capture's own seat, never appended.
    let at = iface["ctorIndex"].as_u64().map(|n| n as usize).unwrap_or(want.len()).min(want.len());
    want.insert(at, "constructor:data:e=0:c=1:w=1".to_string());

    let src = r#"(() => Object.getOwnPropertyNames(HTMLAllCollection.prototype).map(n => {
        const d = Object.getOwnPropertyDescriptor(HTMLAllCollection.prototype, n);
        return (d.get || d.set)
            ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
            : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
    }).join("\n"))()"#;
    assert_eq!(eval(&mut e, "all-proto", src).lines().collect::<Vec<_>>(), want);

    // The two symbols, and the chain. Chrome: @@iterator === Array.prototype.values {w:1,e:0,c:1},
    // @@toStringTag "HTMLAllCollection" {w:0,e:0,c:1}, proto of the prototype = Object.prototype.
    let src = r#"(() => {
        const P = HTMLAllCollection.prototype;
        const i = Object.getOwnPropertyDescriptor(P, Symbol.iterator);
        const t = Object.getOwnPropertyDescriptor(P, Symbol.toStringTag);
        return [i.value === Array.prototype.values, +i.writable, +i.enumerable, +i.configurable,
                t.value, +t.writable, +t.enumerable, +t.configurable,
                Object.getPrototypeOf(P) === Object.prototype,
                HTMLAllCollection.name, HTMLAllCollection.length,
                document.all instanceof HTMLAllCollection,
                Object.getPrototypeOf(document.all) === P].join("|");
    })()"#;
    assert_eq!(
        eval(&mut e, "all-proto-sym", src),
        "true|1|0|1|HTMLAllCollection|0|0|1|true|HTMLAllCollection|0|true|true"
    );
}

/// The INDEXED half, and the split between an index and a name. `document.all[4294967295]` is the
/// element with THAT ID, not an index, because 2^32-1 is not an array index property name —
/// `document.all[4294967294]` (a real index, past the end) is `undefined` even though an element
/// carries that id. WPT asserts both; so does Chrome.
#[tokio::test]
async fn indexed_access_is_tree_order_and_the_index_name_split_is_chromes() {
    let mut e = page().await;
    let src = r##"(() => {
        const A = document.all, D = (x) => x === undefined ? "u" : (x === null ? "n" : (x.tagName + (x.id ? "#"+x.id : "")));
        return [A[0] === document.documentElement, D(A[0]), D(A[1]),
                D(A[-1]), D(A[-0]), D(A[A.length]), D(A[999]),
                D(A["4294967294"]), D(A["4294967295"]), D(A["4294967296"]), D(A["-0"]),
                D(A["043"]), D(A["42"]), D(A["00"]),
                A.length === document.getElementsByTagName("*").length].join("|");
    })()"##;
    assert_eq!(
        eval(&mut e, "all-idx", src),
        // A["42"] is index 42 (out of range) and NOT the <span id=42>; A["043"] is the span.
        "true|HTML#root|HEAD|u|HTML#root|u|u|u|DIV#4294967295|DIV#4294967296|DIV#-0|SPAN#043|u|u|true"
    );

    // The own keys are exactly the indices then the supported names, indices ENUMERABLE and names
    // not -- `[LegacyUnenumerableNamedProperties]`, which is why `Object.keys` is only the indices.
    let src = r#"(() => {
        const A = document.all;
        const idx = Object.keys(A).join(",");
        const want = Array.from({length: A.length}, (_, i) => String(i)).join(",");
        const d0 = Object.getOwnPropertyDescriptor(A, "0"), dn = Object.getOwnPropertyDescriptor(A, "root");
        return [idx === want,
                +d0.enumerable, +d0.configurable, +d0.writable,
                +dn.enumerable, +dn.configurable, +dn.writable,
                Object.getOwnPropertyDescriptor(A, "length") === undefined,
                A.hasOwnProperty("length"), "length" in A].join("|");
    })()"#;
    assert_eq!(eval(&mut e, "all-keys", src), "true|1|1|0|0|1|0|true|false|true");
}

/// The NAMED half. An **id** names any element; a **name** attribute names only an "all"-named
/// element. Measured in Chrome on a page with one of each: `a`, `button`, `embed`, `form`, `iframe`,
/// `img`, `input`, `map`, `meta`, `object`, `select` and `textarea` answer their `name`; `span`,
/// `p`, `div`, `li`, `h1`, `table` and an `<svg name=…>` answer `undefined` — while the `<svg>`'s
/// **id** answers the element.
#[tokio::test]
async fn named_access_follows_the_all_named_element_rule() {
    let mut e = page().await;
    let src = r##"(() => {
        const A = document.all, D = (x) => x === undefined ? "u" : (x === null ? "n" :
            (x.tagName ? x.tagName + (x.id ? "#"+x.id : "") : Object.prototype.toString.call(x) + ":" + x.length));
        return [D(A.root), D(A.tags), D(A.picture), D(A.flags),
                D(A.divwithname), D(A.svgname), D(A.svgid), D(A.nosuch), D(A[""]),
                D(A.foo), D(A.namedItem("foo")), D(A.item("foo")), D(A("foo")),
                A.foo === A.foo, A.foo === A.namedItem("foo")].join("|");
    })()"##;
    assert_eq!(
        eval(&mut e, "all-named", src),
        // `flags` is the <meta name=flags>; `divwithname` and `svgname` are NOT supported names.
        // Two <a name=foo> answer an HTMLCollection of 2 through all four doors -- and it is the SAME
        // object each time, which is Chrome (`document.all.foo === document.all.foo` is true) and is
        // the one WPT subtest Chrome fails ("collections are new live HTMLCollection instances").
        "HTML#root|BODY#tags|IMG|META|u|u|svg#svgid|u|u|\
[object HTMLCollection]:2|[object HTMLCollection]:2|[object HTMLCollection]:2|[object HTMLCollection]:2|true|true"
    );

    // NON_MASKING: an <input name="length"> does not shadow the attribute, but `namedItem` finds it.
    let src = r#"(() => [document.all.length === document.getElementsByTagName("*").length,
                         document.all.namedItem("length").tagName,
                         typeof document.all.item].join("|"))()"#;
    assert_eq!(eval(&mut e, "all-mask", src), "true|INPUT|function");
}

/// `item`, `namedItem` and the LEGACY CALLER, which is the whole reason the object is callable.
///
/// `item(nameOrIndex)` takes an OPTIONAL `DOMString`, so `item()` and `item(undefined)` are the same
/// call and both answer `null`; `namedItem(name)` takes a REQUIRED one, so `namedItem()` is a WebIDL
/// TypeError and `namedItem(undefined)` looks up the string `"undefined"`. All three from WPT, and
/// Chrome agrees with every line.
#[tokio::test]
async fn item_named_item_and_the_legacy_caller_are_chromes() {
    let mut e = page().await;
    let src = r##"(() => {
        const A = document.all, D = (x) => x === undefined ? "u" : (x === null ? "n" :
            (x.tagName ? x.tagName + (x.id ? "#"+x.id : "") : Object.prototype.toString.call(x) + ":" + x.length));
        return [
            D(A.item()), D(A.item(undefined)), D(A.item(null)), D(A.item("")),
            D(A.item(0)), D(A.item("0")), D(A.item("00")), D(A.item("042")), D(A.item("043")),
            D(A.item("4294967294")), D(A.item("4294967295")), D(A.item("-0")), D(A.item(1.7)),
            D(A.item({toString: () => "0"})), D(A.item("picture")), A.item.length,
            D(A.namedItem("0")), D(A.namedItem("42")), D(A.namedItem("4294967294")),
            D(A.namedItem(undefined)), D(A.namedItem(null)), D(A.namedItem("")), A.namedItem.length,
            D(A(0)), D(A("0")), D(A()), D(A(undefined)), D(A(null)), D(A("")), D(A("picture")),
            D(Reflect.apply(A, null, [0])), D(Function.prototype.call.call(A, document.body, "043")),
        ].join("|");
    })()"##;
    assert_eq!(
        eval(&mut e, "all-item", src),
        "n|n|DIV#null|n|HTML#root|HTML#root|n|n|SPAN#043|n|DIV#4294967295|DIV#-0|n|HTML#root|IMG|0|\
n|SPAN#42|DIV#4294967294|DIV#undefined|DIV#null|n|1|\
HTML#root|HTML#root|n|n|DIV#null|n|IMG|HTML#root|SPAN#043"
    );
}

/// The refusals: `namedItem()` is a WebIDL arity TypeError, `document.all` is not a constructor, and
/// the receiver check runs in BOTH directions — `document.all` is not an `HTMLCollection` and
/// `document.images` is not an `HTMLAllCollection`. All four measured in Chrome.
#[tokio::test]
async fn the_refusals_are_chromes_in_both_directions() {
    let mut e = page().await;
    let src = r#"(() => {
        const A = document.all, t = (f) => { try { const v = f(); return "ok:" + typeof v; } catch (e) { return e.constructor.name; } };
        return [
            t(() => A.namedItem()),
            t(() => new A(0)),
            t(() => new (new Proxy(A, { construct: () => ({}) }))),
            t(() => HTMLCollection.prototype.item.call(A, 0)),
            t(() => Object.getOwnPropertyDescriptor(HTMLCollection.prototype, "length").get.call(A)),
            t(() => HTMLAllCollection.prototype.item.call(document.images, 0)),
            t(() => Object.getOwnPropertyDescriptor(HTMLAllCollection.prototype, "length").get.call(document.images)),
            t(() => new HTMLAllCollection()),
            typeof A.call, typeof A.apply, typeof A.bind,
        ].join("|");
    })()"#;
    assert_eq!(
        eval(&mut e, "all-refuse", src),
        "TypeError|TypeError|TypeError|TypeError|TypeError|TypeError|TypeError|TypeError|\
undefined|undefined|undefined"
    );
}

/// IDENTITY and LIVENESS. `document.all === document.all` (Chrome answers the same object every
/// read), the collection tracks the tree, and the sub-collection a repeated name answers is LIVE
/// too. Also the scope: a `<template>`'s content and a shadow root's are not in it.
#[tokio::test]
async fn document_all_is_one_object_and_it_is_live() {
    let mut e = page().await;
    let src = r#"(() => {
        const before = document.all.length;
        const x = document.createElement("b");
        document.body.appendChild(x);
        const during = document.all.length;
        x.remove();
        const after = document.all.length;
        const coll = document.all.foo;
        const a = document.getElementsByName("foo")[0] || document.querySelector("a[name=foo]");
        a.setAttribute("name", "bar");
        const shrunk = coll.length;
        a.setAttribute("name", "foo");
        // Scope: a <template>'s content and a shadow root's are separate trees.
        const tpl = document.createElement("template");
        tpl.innerHTML = '<i id="intpl"></i>';
        document.body.appendChild(tpl);
        const host = document.createElement("div");
        document.body.appendChild(host);
        host.attachShadow({mode: "open"}).innerHTML = '<u id="inshadow"></u>';
        const hidden = [document.all.intpl === undefined, document.all.inshadow === undefined];
        return [document.all === document.all, during - before, after - before,
                shrunk, ...hidden].join("|");
    })()"#;
    assert_eq!(eval(&mut e, "all-live", src), "true|1|0|1|true|true");
}

/// The Document row itself: a getter on `Document.prototype`, enumerable and configurable with no
/// setter — and `document` carries no own `all`. Chrome, measured.
#[tokio::test]
async fn document_all_is_a_prototype_accessor() {
    let mut e = page().await;
    let src = r#"(() => {
        const d = Object.getOwnPropertyDescriptor(Document.prototype, "all");
        return [typeof d.get, typeof d.set, +d.enumerable, +d.configurable,
                "all" in Document.prototype, document.hasOwnProperty("all")].join("|");
    })()"#;
    assert_eq!(eval(&mut e, "all-row", src), "function|undefined|1|1|true|false");
}
